//! Everything the device side keeps in the database: numbered config revisions,
//! the resources they point to, and what devices last reported.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};

use crate::signer::Signer;
use crate::source::Distribution;
use crate::{ConfigDoc, Result};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS config_revisions (
    serial        INTEGER PRIMARY KEY,
    view          TEXT    NOT NULL,
    etag          TEXT    NOT NULL,
    payload       BLOB    NOT NULL,
    key_id        TEXT    NOT NULL,
    signature     BLOB    NOT NULL,
    issued_at_ms  INTEGER NOT NULL,
    source_hash   TEXT    NOT NULL,
    resources     TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS config_revisions_view ON config_revisions (view, serial);
CREATE TABLE IF NOT EXISTS resources (
    sha256      TEXT PRIMARY KEY,
    name        TEXT    NOT NULL,
    version     TEXT    NOT NULL,
    executable  INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    bytes       BLOB    NOT NULL
);
CREATE TABLE IF NOT EXISTS device_reports (
    device_id       TEXT PRIMARY KEY,
    received_ms     INTEGER NOT NULL,
    client_version  TEXT    NOT NULL,
    report          TEXT    NOT NULL
);
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// For devices that present the token: everything, including requests aimed at devices.
    Account,
    /// For devices that do not: the defaults only.
    Public,
}

impl View {
    pub fn as_str(self) -> &'static str {
        match self {
            View::Account => "account",
            View::Public => "public",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Revision {
    pub serial: u64,
    pub view: View,
    pub etag: String,
    pub payload: Vec<u8>,
    pub key_id: String,
    pub signature: Vec<u8>,
    pub issued_at_ms: i64,
    pub source_hash: String,
    pub resources: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct RevisionMeta {
    pub serial: u64,
    pub view: String,
    pub etag: String,
    pub issued_at_ms: i64,
}

#[derive(Clone, Debug)]
pub struct ResourceMeta {
    pub name: String,
    pub version: String,
    pub sha256: String,
    pub executable: bool,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalValue {
    pub key: String,
    pub value_json: String,
    pub locked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestState {
    pub id: String,
    /// `pending`, `accepted` or `rejected`.
    pub state: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub serial: u64,
    pub reason: String,
}

/// What a device last told us (docs/api.md, devices).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceState {
    pub received_ms: i64,
    pub client_version: String,
    pub applied_serial: u64,
    pub applied_etag: String,
    pub exec_allowed: bool,
    pub local_values: Vec<LocalValue>,
    pub requests: Vec<RequestState>,
    pub last_failure: Option<Failure>,
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

fn rfc3339(ms: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .expect("a time we made")
        .format(&time::format_description::well_known::Rfc3339)
        .expect("formattable")
}

fn revision(row: &sqlx::sqlite::SqliteRow) -> Revision {
    let view: String = row.get("view");
    let resources: String = row.get("resources");
    Revision {
        serial: row.get::<i64, _>("serial") as u64,
        view: if view == "public" { View::Public } else { View::Account },
        etag: row.get("etag"),
        payload: row.get("payload"),
        key_id: row.get("key_id"),
        signature: row.get("signature"),
        issued_at_ms: row.get("issued_at_ms"),
        source_hash: row.get("source_hash"),
        resources: serde_json::from_str(&resources).unwrap_or_default(),
    }
}

impl Store {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::raw_sql(SCHEMA).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn latest(&self, view: View) -> Result<Option<Revision>> {
        let row = sqlx::query("SELECT * FROM config_revisions WHERE view = ? ORDER BY serial DESC LIMIT 1")
            .bind(view.as_str())
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(revision))
    }

    /// The newest revision of either view; what the config directory was like when it was made.
    pub async fn newest(&self) -> Result<Option<Revision>> {
        let row = sqlx::query("SELECT * FROM config_revisions ORDER BY serial DESC LIMIT 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(revision))
    }

    pub async fn history(&self, limit: u32) -> Result<Vec<RevisionMeta>> {
        let rows =
            sqlx::query("SELECT serial, view, etag, issued_at_ms FROM config_revisions ORDER BY serial DESC LIMIT ?")
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .iter()
            .map(|r| RevisionMeta {
                serial: r.get::<i64, _>("serial") as u64,
                view: r.get("view"),
                etag: r.get("etag"),
                issued_at_ms: r.get("issued_at_ms"),
            })
            .collect())
    }

    /// Signs and stores a new pair of revisions: the public view takes the next
    /// serial and the account view the one after.
    pub async fn publish(&self, dist: &Distribution, signer: &dyn Signer, now_ms: i64) -> Result<(Revision, Revision)> {
        let mut tx = self.pool.begin().await?;
        let max: Option<i64> = sqlx::query_scalar("SELECT MAX(serial) FROM config_revisions")
            .fetch_one(&mut *tx)
            .await?;
        let public_serial = max.unwrap_or(0) as u64 + 1;
        let issued_at = rfc3339(now_ms);
        let key_id = signer.key_id();
        let source_hash = format!("{}:{}", dist.source_hash, key_id);

        for r in &dist.resources {
            sqlx::query("INSERT OR IGNORE INTO resources (sha256, name, version, executable, size, bytes) VALUES (?, ?, ?, ?, ?, ?)")
                .bind(&r.sha256)
                .bind(&r.name)
                .bind(&r.version)
                .bind(r.executable)
                .bind(r.bytes.len() as i64)
                .bind(&r.bytes)
                .execute(&mut *tx)
                .await?;
        }

        let mut made = Vec::new();
        for (view, serial) in [(View::Public, public_serial), (View::Account, public_serial + 1)] {
            let mut doc: ConfigDoc = match view {
                View::Account => dist.doc.clone(),
                View::Public => dist.doc.public_view(),
            };
            doc.serial = serial;
            doc.issued_at = issued_at.clone();
            let payload = doc.to_payload();
            let signed = signer.sign(&payload);
            let rev = Revision {
                serial,
                view,
                etag: hex::encode(Sha256::digest(&payload)),
                payload,
                key_id: signed.key_id,
                signature: signed.signature,
                issued_at_ms: now_ms,
                source_hash: source_hash.clone(),
                resources: doc.resource_shas(),
            };
            sqlx::query(
                "INSERT INTO config_revisions (serial, view, etag, payload, key_id, signature, issued_at_ms, source_hash, resources) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(serial as i64)
            .bind(view.as_str())
            .bind(&rev.etag)
            .bind(&rev.payload)
            .bind(&rev.key_id)
            .bind(&rev.signature)
            .bind(now_ms)
            .bind(&rev.source_hash)
            .bind(serde_json::to_string(&rev.resources).expect("strings"))
            .execute(&mut *tx)
            .await?;
            made.push(rev);
        }
        tx.commit().await?;
        let account = made.pop().expect("two revisions");
        let public = made.pop().expect("two revisions");
        Ok((account, public))
    }

    pub async fn resource_bytes(&self, sha256: &str) -> Result<Option<Vec<u8>>> {
        Ok(sqlx::query_scalar("SELECT bytes FROM resources WHERE sha256 = ?")
            .bind(sha256)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn resource_metas(&self, shas: &[String]) -> Result<Vec<ResourceMeta>> {
        let mut out = Vec::new();
        for sha in shas {
            if let Some(row) = sqlx::query("SELECT name, version, executable, size FROM resources WHERE sha256 = ?")
                .bind(sha)
                .fetch_optional(&self.pool)
                .await?
            {
                out.push(ResourceMeta {
                    name: row.get("name"),
                    version: row.get("version"),
                    sha256: sha.clone(),
                    executable: row.get("executable"),
                    size: row.get::<i64, _>("size") as u64,
                });
            }
        }
        Ok(out)
    }

    pub async fn upsert_report(&self, device_id: &str, state: &DeviceState) -> Result<()> {
        sqlx::query(
            "INSERT INTO device_reports (device_id, received_ms, client_version, report) VALUES (?, ?, ?, ?) \
             ON CONFLICT(device_id) DO UPDATE SET received_ms = excluded.received_ms, \
             client_version = excluded.client_version, report = excluded.report",
        )
        .bind(device_id)
        .bind(state.received_ms)
        .bind(&state.client_version)
        .bind(serde_json::to_string(state).expect("plain struct"))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn device_states(&self) -> Result<Vec<(String, DeviceState)>> {
        let rows = sqlx::query("SELECT device_id, report FROM device_reports ORDER BY device_id")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                let state: DeviceState = serde_json::from_str(&r.get::<String, _>("report")).ok()?;
                Some((r.get("device_id"), state))
            })
            .collect())
    }
}
