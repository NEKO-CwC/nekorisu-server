//! Synthetic archives for tests and local development.
//!
//! Everything here is invented. The server repository is public, so no real
//! session text may ever be put in a fixture. The exact JSON field
//! names are provisional until the client writes uploads, so this is the one
//! place that writes them.

pub mod claude;
pub mod codex;

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const ROOT: &str = "nekorisu/v1";

/// One line of a transcript, with what the line map records about it.
#[derive(Clone, Debug)]
pub struct Line {
    /// `user`, `assistant`, `reasoning`, `tool` or `meta`.
    pub kind: &'static str,
    pub raw_type: String,
    pub ts_ms: i64,
    pub json: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

pub fn rfc3339(ms: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).expect("in range");
    t.format(&time::format_description::well_known::Rfc3339)
        .expect("formattable")
}

pub struct FileSpec {
    /// Relative to the agent's home, e.g. `projects/-p/sid.jsonl`.
    pub rel_path: String,
    /// `transcript`, `subagent`, `tool_result`, ...
    pub role: &'static str,
    /// Each inner Vec is one uploaded segment, in order.
    pub segments: Vec<Vec<Line>>,
}

impl FileSpec {
    pub fn transcript(rel_path: &str, lines: Vec<Line>) -> Self {
        Self {
            rel_path: rel_path.to_string(),
            role: "transcript",
            segments: vec![lines],
        }
    }
}

pub struct SessionSpec {
    pub agent: &'static str,
    pub id: String,
    pub device: String,
    pub seq: u64,
    pub parent_manifest: Option<String>,
    pub created_ms: i64,
    pub cwd: String,
    pub title: Option<String>,
    pub agent_version: String,
    pub files: Vec<FileSpec>,
    pub parent_session: Option<String>,
    pub fork_from: Option<(String, u64)>,
    pub rewrite: bool,
}

impl SessionSpec {
    pub fn new(agent: &'static str, id: &str, device: &str, created_ms: i64, files: Vec<FileSpec>) -> Self {
        Self {
            agent,
            id: id.to_string(),
            device: device.to_string(),
            seq: 1,
            parent_manifest: None,
            created_ms,
            cwd: "/work/demo".to_string(),
            title: None,
            agent_version: "1.0.0".to_string(),
            files,
            parent_session: None,
            fork_from: None,
            rewrite: false,
        }
    }
}

#[derive(Default)]
pub struct ArchiveBuilder {
    files: BTreeMap<String, Vec<u8>>,
    log_lines: BTreeMap<String, Vec<String>>,
    log_seq: BTreeMap<String, u64>,
}

impl ArchiveBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&mut self, path: &str, body: impl Into<Vec<u8>>) -> &mut Self {
        self.files.insert(format!("{ROOT}/{path}"), body.into());
        self
    }

    pub fn device(&mut self, id: &str, name: &str, created_ms: i64) -> &mut Self {
        let body = json!({ "id": id, "name": name, "created_at_ms": created_ms });
        self.put(&format!("devices/{id}.json"), body.to_string())
    }

    pub fn digest(&mut self, id: &str, markdown: &str, sessions: &[&str], updated_ms: i64) -> &mut Self {
        let refs: Vec<Value> = sessions.iter().map(|s| json!({ "session": s })).collect();
        self.put(&format!("digests/{id}.md"), markdown).put(
            &format!("digests/{id}.json"),
            json!({ "id": id, "refs": refs, "updated_at_ms": updated_ms }).to_string(),
        )
    }

    /// Writes the segments, line maps and manifest of one upload, and an
    /// `archive` record in the uploading device's log.
    pub fn session(&mut self, spec: SessionSpec) -> &mut Self {
        let dir = format!("sessions/{}/{}", spec.agent, spec.id);
        let mut manifest_files = Vec::new();
        let mut total_bytes = 0u64;
        let (mut first, mut last) = (i64::MAX, 0i64);
        for file in &spec.files {
            let mut start = 0u64;
            let mut lines_total = 0u64;
            let mut segments = Vec::new();
            for segment in &file.segments {
                let mut bytes = Vec::new();
                let mut rows = Vec::new();
                let mut raw_types: Vec<String> = Vec::new();
                for line in segment {
                    let raw = match raw_types.iter().position(|t| *t == line.raw_type) {
                        Some(i) => i,
                        None => {
                            raw_types.push(line.raw_type.clone());
                            raw_types.len() - 1
                        }
                    };
                    rows.push(json!([bytes.len(), line.json.len(), line.kind, raw, line.ts_ms]));
                    bytes.extend_from_slice(line.json.as_bytes());
                    bytes.push(b'\n');
                    if line.ts_ms > 0 {
                        first = first.min(line.ts_ms);
                        last = last.max(line.ts_ms);
                    }
                }
                let sha = hex::encode(Sha256::digest(&bytes));
                let linemap = json!({ "format": "nekorisu.linemap/1", "raw_types": raw_types, "lines": rows });
                self.put(&format!("{dir}/linemaps/{sha}.json"), linemap.to_string());
                self.put(
                    &format!("{dir}/files/{}/{start}-{sha}.seg", file.rel_path),
                    bytes.clone(),
                );
                segments.push(json!({ "start": start, "len": bytes.len(), "sha256": sha, "lines": segment.len() }));
                start += bytes.len() as u64;
                lines_total += segment.len() as u64;
            }
            total_bytes += start;
            manifest_files.push(json!({
                "path": file.rel_path, "role": file.role, "size": start, "sha256": "",
                "mtime_ms": 0, "form": "as-is", "segments": segments, "lines": lines_total,
            }));
        }
        if first == i64::MAX {
            first = spec.created_ms;
            last = spec.created_ms;
        }
        let manifest_id = format!("{}-{}", spec.device, spec.seq);
        let manifest = json!({
            "format": "nekorisu.manifest/1",
            "id": manifest_id,
            "parent": spec.parent_manifest,
            "session": {
                "agent": spec.agent, "id": spec.id, "agent_version": spec.agent_version,
                "project": spec.cwd.replace('/', "-"), "cwd": spec.cwd, "title": spec.title,
                "first_ts_ms": first, "last_ts_ms": last,
            },
            "device": spec.device,
            "created_at_ms": spec.created_ms,
            "files": manifest_files,
            "links": {
                "parent": spec.parent_session,
                "children": [],
                "fork_from": spec.fork_from.as_ref().map(|(s, b)| json!({ "session": s, "byte": b })),
            },
            "rewrite": spec.rewrite,
            "encryption": "none",
        });
        let manifest_path = format!("{dir}/manifests/{manifest_id}.json");
        self.put(&manifest_path, manifest.to_string());

        let seq = self.log_seq.entry(spec.device.clone()).or_insert(0);
        *seq += 1;
        let record = json!({
            "seq": *seq, "device": spec.device, "at_ms": spec.created_ms, "op": "archive",
            "sessions": [format!("{}:{}", spec.agent, spec.id)], "tags": [], "note": null,
            "detail": { "manifest": format!("{ROOT}/{manifest_path}"), "bytes": total_bytes },
        });
        self.log_lines
            .entry(spec.device.clone())
            .or_default()
            .push(record.to_string());
        self
    }

    /// An operation-log record that is not an upload, e.g. a note.
    pub fn log_record(&mut self, device: &str, record: Value) -> &mut Self {
        self.log_lines
            .entry(device.to_string())
            .or_default()
            .push(record.to_string());
        self
    }

    fn finish_logs(&mut self) {
        let logs = std::mem::take(&mut self.log_lines);
        for (device, lines) in logs {
            let mut body = lines.join("\n");
            body.push('\n');
            self.put(&format!("logs/{device}/1.jsonl"), body);
        }
    }

    pub fn into_memory(mut self) -> nekorisu_storage::MemoryProvider {
        self.finish_logs();
        let memory = nekorisu_storage::MemoryProvider::new();
        for (path, body) in self.files {
            memory.put(&path, bytes::Bytes::from(body));
        }
        memory
    }

    /// Writes the layout to disk, e.g. to serve it with a WebDAV server.
    pub fn write_to_dir(mut self, root: &Path) -> io::Result<()> {
        self.finish_logs();
        for (path, body) in self.files {
            let full = root.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(full, body)?;
        }
        Ok(())
    }
}
