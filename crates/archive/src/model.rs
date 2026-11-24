use std::fmt;

use serde_json::Value;

use crate::json;
use crate::{Error, Result};

/// `<agent>:<id>`, the text form of a session.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionRef {
    pub agent: String,
    pub id: String,
}

impl SessionRef {
    pub fn parse(text: &str) -> Option<Self> {
        let (agent, id) = text.split_once(':')?;
        (!agent.is_empty() && !id.is_empty()).then(|| Self {
            agent: agent.to_string(),
            id: id.to_string(),
        })
    }

    /// Like `parse`, but a bare id belongs to `default_agent`.
    pub fn parse_or(text: &str, default_agent: &str) -> Option<Self> {
        Self::parse(text).or_else(|| {
            (!text.is_empty()).then(|| Self {
                agent: default_agent.to_string(),
                id: text.to_string(),
            })
        })
    }
}

impl fmt::Display for SessionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.agent, self.id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Transcript,
    Subagent,
    ToolResult,
    FileHistory,
    Asset,
    Other,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Transcript => "transcript",
            Role::Subagent => "subagent",
            Role::ToolResult => "tool_result",
            Role::FileHistory => "file_history",
            Role::Asset => "asset",
            Role::Other => "other",
        }
    }

    fn parse(s: &str) -> Role {
        match s.replace('-', "_").to_ascii_lowercase().as_str() {
            "transcript" | "main" => Role::Transcript,
            "subagent" | "sub_agent" => Role::Subagent,
            "tool_result" | "tool_output" | "tool_results" => Role::ToolResult,
            "file_history" => Role::FileHistory,
            "asset" | "attachment" | "image" => Role::Asset,
            _ => Role::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentRef {
    pub start: u64,
    pub len: u64,
    pub sha256: String,
    pub lines: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct ManifestFile {
    /// Relative to the agent's home directory.
    pub path: String,
    pub role: Role,
    pub size: u64,
    pub lines: Option<u64>,
    /// In byte order.
    pub segments: Vec<SegmentRef>,
}

#[derive(Clone, Debug, Default)]
pub struct SessionMeta {
    pub agent: String,
    pub id: String,
    pub agent_version: Option<String>,
    pub project: Option<String>,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub first_ts_ms: Option<i64>,
    pub last_ts_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkFrom {
    pub session: String,
    pub byte: u64,
}

#[derive(Clone, Debug)]
pub struct Manifest {
    pub id: String,
    pub parent: Option<String>,
    pub session: SessionMeta,
    pub device: String,
    pub created_ms: i64,
    pub files: Vec<ManifestFile>,
    /// The parent session (a Codex sub-agent thread), as `<agent>:<id>`.
    pub parent_session: Option<String>,
    pub fork_from: Option<ForkFrom>,
    pub rewrite: bool,
}

impl Manifest {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// `agent` and `session_id` come from the directory the file was found in
    /// and fill in whatever the file leaves out.
    pub fn from_json(bytes: &[u8], file_stem: &str, agent: &str, session_id: &str) -> Result<Self> {
        let v: Value = serde_json::from_slice(bytes).map_err(|e| Error::format("manifest", e.to_string()))?;
        if !v.is_object() {
            return Err(Error::format("manifest", "not an object"));
        }
        let empty = Value::Null;
        let s = json::get(&v, &["session"]).unwrap_or(&empty);
        let session = SessionMeta {
            agent: json::string(s, &["agent"]).unwrap_or_else(|| agent.to_string()),
            id: json::string(s, &["id"]).unwrap_or_else(|| session_id.to_string()),
            agent_version: json::string(s, &["agent_version", "version"]),
            project: json::string(s, &["project"]),
            cwd: json::string(s, &["cwd"]),
            title: json::string(s, &["title"]),
            first_ts_ms: json::time_field(s, &["first_ts_ms", "first_ts", "first_at", "started_at"]),
            last_ts_ms: json::time_field(s, &["last_ts_ms", "last_ts", "last_at", "ended_at"]),
        };
        let files = json::get(&v, &["files"])
            .and_then(Value::as_array)
            .map(|a| a.iter().map(parse_file).collect())
            .unwrap_or_default();
        let links = json::get(&v, &["links"]).unwrap_or(&empty);
        let fork_from = json::get(links, &["fork_from"]).and_then(|f| {
            Some(ForkFrom {
                session: json::string(f, &["session"])?,
                byte: json::uint(f, &["byte", "offset"]).unwrap_or(0),
            })
        });
        Ok(Manifest {
            id: json::string(&v, &["id"]).unwrap_or_else(|| file_stem.to_string()),
            parent: json::string(&v, &["parent"]),
            session,
            device: json::string(&v, &["device", "device_id"]).unwrap_or_default(),
            created_ms: json::time_field(&v, &["created_at_ms", "created_at", "created_ms"]).unwrap_or(0),
            files,
            parent_session: json::string(links, &["parent"]),
            fork_from,
            rewrite: json::boolean(&v, &["rewrite"]),
        })
    }
}

fn parse_file(v: &Value) -> ManifestFile {
    let mut segments: Vec<SegmentRef> = json::get(v, &["segments"])
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|s| SegmentRef {
                    start: json::uint(s, &["start", "offset"]).unwrap_or(0),
                    len: json::uint(s, &["len", "length"]).unwrap_or(0),
                    sha256: json::string(s, &["sha256", "sha"]).unwrap_or_default(),
                    lines: json::uint(s, &["lines"]),
                })
                .collect()
        })
        .unwrap_or_default();
    segments.sort_by_key(|s| s.start);
    let size = json::uint(v, &["size", "bytes"]).unwrap_or_else(|| segments.iter().map(|s| s.len).sum());
    ManifestFile {
        path: json::string(v, &["path"]).unwrap_or_default(),
        role: json::string(v, &["role"])
            .map(|r| Role::parse(&r))
            .unwrap_or(Role::Other),
        size,
        lines: json::uint(v, &["lines", "total_lines"]),
        segments,
    }
}

#[derive(Clone, Debug)]
pub struct DigestInfo {
    pub id: String,
    pub updated_ms: Option<i64>,
    /// The sessions it refers to, as `<agent>:<id>`.
    pub sessions: Vec<String>,
}

impl DigestInfo {
    pub fn from_json(bytes: &[u8], file_stem: &str) -> Result<Self> {
        let v: Value = serde_json::from_slice(bytes).map_err(|e| Error::format("digest", e.to_string()))?;
        let sessions = json::get(&v, &["refs"])
            .and_then(Value::as_array)
            .map(|refs| {
                refs.iter()
                    .filter_map(|r| match r {
                        Value::String(s) => Some(s.clone()),
                        other => json::string(other, &["session"]),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(DigestInfo {
            id: json::string(&v, &["id"]).unwrap_or_else(|| file_stem.to_string()),
            updated_ms: json::time_field(&v, &["updated_at_ms", "updated_at", "updated_ms"]),
            sessions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_refs() {
        let r = SessionRef::parse("claude-code:abc-1").unwrap();
        assert_eq!((r.agent.as_str(), r.id.as_str()), ("claude-code", "abc-1"));
        assert_eq!(r.to_string(), "claude-code:abc-1");
        assert!(SessionRef::parse("nocolon").is_none());
        assert_eq!(SessionRef::parse_or("x", "codex").unwrap().to_string(), "codex:x");
    }

    #[test]
    fn manifest_fills_gaps_from_the_directory() {
        let m = Manifest::from_json(
            b"{\"files\":[{\"path\":\"a\",\"role\":\"tool-output\"}]}",
            "d1-2",
            "codex",
            "t9",
        )
        .unwrap();
        assert_eq!(m.id, "d1-2");
        assert_eq!(m.session.agent, "codex");
        assert_eq!(m.session.id, "t9");
        assert_eq!(m.files[0].role, Role::ToolResult);
        assert!(Manifest::from_json(b"[]", "x", "a", "b").is_err());
    }
}
