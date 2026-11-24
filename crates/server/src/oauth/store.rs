//! What OAuth keeps in SQLite. Codes and tokens are stored only as SHA-256
//! hashes, so the database alone cannot be used to call the MCP endpoint.
//!
//! A grant is one approval on the consent page: one app, one redirect URI,
//! one resource and scope. Its code, access tokens and refresh tokens all
//! point at it; revoking it deletes them all.

use sqlx::{Row, SqlitePool};

use super::client::Client;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS oauth_clients (
    client_id     TEXT PRIMARY KEY,
    client_name   TEXT    NOT NULL,
    redirect_uris TEXT    NOT NULL,
    created_ms    INTEGER NOT NULL,
    approved_ms   INTEGER
);
CREATE TABLE IF NOT EXISTS oauth_grants (
    id           TEXT PRIMARY KEY,
    client_id    TEXT    NOT NULL,
    client_name  TEXT    NOT NULL,
    redirect_uri TEXT    NOT NULL,
    resource     TEXT    NOT NULL,
    scope        TEXT    NOT NULL,
    created_ms   INTEGER NOT NULL,
    last_used_ms INTEGER
);
CREATE TABLE IF NOT EXISTS oauth_codes (
    code_hash      TEXT PRIMARY KEY,
    grant_id       TEXT    NOT NULL,
    code_challenge TEXT    NOT NULL,
    expires_ms     INTEGER NOT NULL,
    used_ms        INTEGER
);
CREATE TABLE IF NOT EXISTS oauth_tokens (
    token_hash TEXT PRIMARY KEY,
    grant_id   TEXT    NOT NULL,
    kind       TEXT    NOT NULL CHECK (kind IN ('access', 'refresh')),
    expires_ms INTEGER NOT NULL,
    used_ms    INTEGER
);
CREATE INDEX IF NOT EXISTS oauth_tokens_by_grant ON oauth_tokens (grant_id);
CREATE INDEX IF NOT EXISTS oauth_codes_by_grant ON oauth_codes (grant_id);
"#;

/// Registered clients nobody approved are forgotten after a day…
const UNAPPROVED_CLIENT_MS: i64 = 24 * 3600 * 1000;
/// …and at most this many are kept, the oldest going first, so registering
/// in a loop cannot fill the disk.
const MAX_UNAPPROVED_CLIENTS: i64 = 500;
/// A grant whose code was never redeemed is dropped after this long.
const UNREDEEMED_GRANT_MS: i64 = 3600 * 1000;
/// `last_used_ms` is written at most this often per grant.
const TOUCH_EVERY_MS: i64 = 60 * 1000;

pub struct NewGrant<'a> {
    pub id: &'a str,
    pub client_id: &'a str,
    pub client_name: &'a str,
    pub redirect_uri: &'a str,
    pub resource: &'a str,
    pub scope: &'a str,
}

/// A code, with what its grant was given for.
pub struct CodeRow {
    pub grant_id: String,
    pub code_challenge: String,
    pub expires_ms: i64,
    pub used_ms: Option<i64>,
    pub client_id: String,
    pub redirect_uri: String,
    pub resource: String,
    pub scope: String,
}

pub struct RefreshRow {
    pub grant_id: String,
    pub expires_ms: i64,
    pub used_ms: Option<i64>,
    pub client_id: String,
    pub resource: String,
    pub scope: String,
}

pub struct AccessRow {
    pub grant_id: String,
    pub resource: String,
    pub scope: String,
    pub last_used_ms: Option<i64>,
}

/// One authorised app, for the panel.
pub struct GrantRow {
    pub id: String,
    pub client_id: String,
    pub client_name: String,
    pub redirect_uri: String,
    pub scope: String,
    pub created_ms: i64,
    pub last_used_ms: Option<i64>,
}

/// A new access token and refresh token, as hashes, with their expiry.
pub struct NewTokens<'a> {
    pub access_hash: &'a str,
    pub access_expires_ms: i64,
    pub refresh_hash: &'a str,
    pub refresh_expires_ms: i64,
}

pub struct Db {
    pool: SqlitePool,
}

impl Db {
    pub async fn new(pool: SqlitePool) -> Result<Self, sqlx::Error> {
        sqlx::raw_sql(SCHEMA).execute(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn insert_client(&self, client: &Client, now: i64) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM oauth_clients WHERE approved_ms IS NULL AND created_ms < ?")
            .bind(now - UNAPPROVED_CLIENT_MS)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "DELETE FROM oauth_clients WHERE client_id IN (SELECT client_id FROM oauth_clients \
             WHERE approved_ms IS NULL ORDER BY created_ms DESC LIMIT -1 OFFSET ?)",
        )
        .bind(MAX_UNAPPROVED_CLIENTS - 1)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "INSERT INTO oauth_clients (client_id, client_name, redirect_uris, created_ms) VALUES (?, ?, ?, ?)",
        )
        .bind(&client.id)
        .bind(&client.name)
        .bind(serde_json::to_string(&client.redirect_uris).unwrap_or_else(|_| "[]".to_string()))
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn client(&self, id: &str) -> Result<Option<Client>, sqlx::Error> {
        let row = sqlx::query("SELECT client_name, redirect_uris FROM oauth_clients WHERE client_id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| Client {
            id: id.to_string(),
            name: r.get("client_name"),
            redirect_uris: serde_json::from_str(r.get::<&str, _>("redirect_uris")).unwrap_or_default(),
        }))
    }

    /// Clears out what has expired: codes, tokens, grants left with nothing,
    /// and registered clients nobody approved.
    pub async fn housekeep(&self, now: i64) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM oauth_codes WHERE expires_ms < ?")
            .bind(now)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM oauth_tokens WHERE expires_ms < ?")
            .bind(now)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "DELETE FROM oauth_grants WHERE created_ms < ? \
             AND NOT EXISTS (SELECT 1 FROM oauth_tokens t WHERE t.grant_id = oauth_grants.id) \
             AND NOT EXISTS (SELECT 1 FROM oauth_codes c WHERE c.grant_id = oauth_grants.id)",
        )
        .bind(now - UNREDEEMED_GRANT_MS)
        .execute(&self.pool)
        .await?;
        sqlx::query("DELETE FROM oauth_clients WHERE approved_ms IS NULL AND created_ms < ?")
            .bind(now - UNAPPROVED_CLIENT_MS)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Records an approval: the grant and its code.
    pub async fn approve(
        &self,
        grant: &NewGrant<'_>,
        code_hash: &str,
        code_challenge: &str,
        code_expires_ms: i64,
        now: i64,
    ) -> Result<(), sqlx::Error> {
        self.housekeep(now).await?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO oauth_grants (id, client_id, client_name, redirect_uri, resource, scope, created_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(grant.id)
        .bind(grant.client_id)
        .bind(grant.client_name)
        .bind(grant.redirect_uri)
        .bind(grant.resource)
        .bind(grant.scope)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO oauth_codes (code_hash, grant_id, code_challenge, expires_ms) VALUES (?, ?, ?, ?)")
            .bind(code_hash)
            .bind(grant.id)
            .bind(code_challenge)
            .bind(code_expires_ms)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE oauth_clients SET approved_ms = ? WHERE client_id = ?")
            .bind(now)
            .bind(grant.client_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }

    pub async fn code(&self, code_hash: &str) -> Result<Option<CodeRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT c.grant_id, c.code_challenge, c.expires_ms, c.used_ms, \
                    g.client_id, g.redirect_uri, g.resource, g.scope \
             FROM oauth_codes c JOIN oauth_grants g ON g.id = c.grant_id WHERE c.code_hash = ?",
        )
        .bind(code_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| CodeRow {
            grant_id: r.get("grant_id"),
            code_challenge: r.get("code_challenge"),
            expires_ms: r.get("expires_ms"),
            used_ms: r.get("used_ms"),
            client_id: r.get("client_id"),
            redirect_uri: r.get("redirect_uri"),
            resource: r.get("resource"),
            scope: r.get("scope"),
        }))
    }

    /// Marks the code used and issues the first tokens, in one transaction.
    /// False when the code was used meanwhile (another request won).
    pub async fn redeem_code(
        &self,
        code_hash: &str,
        grant_id: &str,
        tokens: &NewTokens<'_>,
        now: i64,
    ) -> Result<bool, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let used = sqlx::query("UPDATE oauth_codes SET used_ms = ? WHERE code_hash = ? AND used_ms IS NULL")
            .bind(now)
            .bind(code_hash)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if used != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        insert_tokens(&mut tx, grant_id, tokens, now).await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn refresh(&self, token_hash: &str) -> Result<Option<RefreshRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT t.grant_id, t.expires_ms, t.used_ms, g.client_id, g.resource, g.scope \
             FROM oauth_tokens t JOIN oauth_grants g ON g.id = t.grant_id \
             WHERE t.token_hash = ? AND t.kind = 'refresh'",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| RefreshRow {
            grant_id: r.get("grant_id"),
            expires_ms: r.get("expires_ms"),
            used_ms: r.get("used_ms"),
            client_id: r.get("client_id"),
            resource: r.get("resource"),
            scope: r.get("scope"),
        }))
    }

    /// Retires a refresh token and issues the next pair, in one transaction.
    /// The retired one is kept (marked used) until it expires, so presenting
    /// it again is recognised as reuse. False when it was retired meanwhile.
    pub async fn rotate(
        &self,
        old_hash: &str,
        grant_id: &str,
        tokens: &NewTokens<'_>,
        now: i64,
    ) -> Result<bool, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let used = sqlx::query(
            "UPDATE oauth_tokens SET used_ms = ? WHERE token_hash = ? AND kind = 'refresh' AND used_ms IS NULL",
        )
        .bind(now)
        .bind(old_hash)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if used != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        insert_tokens(&mut tx, grant_id, tokens, now).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// The grant behind a live access token.
    pub async fn access(&self, token_hash: &str, now: i64) -> Result<Option<AccessRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT t.grant_id, g.resource, g.scope, g.last_used_ms \
             FROM oauth_tokens t JOIN oauth_grants g ON g.id = t.grant_id \
             WHERE t.token_hash = ? AND t.kind = 'access' AND t.expires_ms > ?",
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| AccessRow {
            grant_id: r.get("grant_id"),
            resource: r.get("resource"),
            scope: r.get("scope"),
            last_used_ms: r.get("last_used_ms"),
        }))
    }

    /// Notes that a grant was used, at most once a minute.
    pub async fn touch(&self, grant_id: &str, last_used_ms: Option<i64>, now: i64) -> Result<(), sqlx::Error> {
        if last_used_ms.is_some_and(|t| now - t < TOUCH_EVERY_MS) {
            return Ok(());
        }
        sqlx::query("UPDATE oauth_grants SET last_used_ms = ? WHERE id = ?")
            .bind(now)
            .bind(grant_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Deletes a grant with its codes and tokens. False if there was none.
    pub async fn revoke(&self, grant_id: &str) -> Result<bool, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM oauth_tokens WHERE grant_id = ?")
            .bind(grant_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM oauth_codes WHERE grant_id = ?")
            .bind(grant_id)
            .execute(&mut *tx)
            .await?;
        let deleted = sqlx::query("DELETE FROM oauth_grants WHERE id = ?")
            .bind(grant_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(deleted > 0)
    }

    /// The grants that can still be used: those with a live refresh token.
    pub async fn grants(&self, now: i64) -> Result<Vec<GrantRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT g.id, g.client_id, g.client_name, g.redirect_uri, g.scope, g.created_ms, g.last_used_ms \
             FROM oauth_grants g WHERE EXISTS (SELECT 1 FROM oauth_tokens t WHERE t.grant_id = g.id \
             AND t.kind = 'refresh' AND t.used_ms IS NULL AND t.expires_ms > ?) \
             ORDER BY g.created_ms DESC",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| GrantRow {
                id: r.get("id"),
                client_id: r.get("client_id"),
                client_name: r.get("client_name"),
                redirect_uri: r.get("redirect_uri"),
                scope: r.get("scope"),
                created_ms: r.get("created_ms"),
                last_used_ms: r.get("last_used_ms"),
            })
            .collect())
    }
}

async fn insert_tokens(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    grant_id: &str,
    tokens: &NewTokens<'_>,
    now: i64,
) -> Result<(), sqlx::Error> {
    for (hash, kind, expires) in [
        (tokens.access_hash, "access", tokens.access_expires_ms),
        (tokens.refresh_hash, "refresh", tokens.refresh_expires_ms),
    ] {
        sqlx::query("INSERT INTO oauth_tokens (token_hash, grant_id, kind, expires_ms) VALUES (?, ?, ?, ?)")
            .bind(hash)
            .bind(grant_id)
            .bind(kind)
            .bind(expires)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("UPDATE oauth_grants SET last_used_ms = ? WHERE id = ?")
        .bind(now)
        .bind(grant_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
