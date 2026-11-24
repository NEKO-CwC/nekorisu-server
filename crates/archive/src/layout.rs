//! Paths in the remote layout. Every path is built here, from components that
//! have been checked, so nothing read from the storage can steer a read
//! somewhere else.

use crate::{Error, Result};

pub const ROOT: &str = "nekorisu/v1";

/// One path component: no slash, no dots-only name, no control characters.
pub fn component(s: &str) -> Result<&str> {
    if s.is_empty() || s == "." || s == ".." || s.contains(['/', '\\']) || s.chars().any(char::is_control) {
        return Err(Error::BadRequest(format!("not a valid name: {s:?}")));
    }
    Ok(s)
}

pub fn devices_dir() -> String {
    format!("{ROOT}/devices")
}

pub fn logs_dir() -> String {
    format!("{ROOT}/logs")
}

pub fn digests_dir() -> String {
    format!("{ROOT}/digests")
}

pub fn sessions_dir() -> String {
    format!("{ROOT}/sessions")
}

pub fn session_dir(agent: &str, id: &str) -> Result<String> {
    Ok(format!("{ROOT}/sessions/{}/{}", component(agent)?, component(id)?))
}

pub fn manifests_dir(agent: &str, id: &str) -> Result<String> {
    Ok(format!("{}/manifests", session_dir(agent, id)?))
}

pub fn linemap_path(agent: &str, id: &str, sha256: &str) -> Result<String> {
    Ok(format!(
        "{}/linemaps/{}.json",
        session_dir(agent, id)?,
        component(sha256)?
    ))
}

/// `rel` is a path inside the agent's home; each part is checked.
pub fn segment_path(agent: &str, id: &str, rel: &str, start: u64, sha256: &str) -> Result<String> {
    let mut path = format!("{}/files", session_dir(agent, id)?);
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        path.push('/');
        path.push_str(component(part)?);
    }
    Ok(format!("{path}/{start}-{}.seg", component(sha256)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_paths() {
        assert_eq!(
            segment_path("codex", "t1", "sessions/2026/a.jsonl", 0, "abc").unwrap(),
            "nekorisu/v1/sessions/codex/t1/files/sessions/2026/a.jsonl/0-abc.seg"
        );
    }

    #[test]
    fn refuses_steering() {
        assert!(session_dir("codex", "../x").is_err());
        assert!(segment_path("codex", "t1", "a/../b", 0, "abc").is_err());
        assert!(linemap_path("codex", "t1", "a/b").is_err());
    }
}
