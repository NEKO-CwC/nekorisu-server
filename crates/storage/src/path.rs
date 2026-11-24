use crate::{Error, Result};

/// Normalizes a path relative to the storage root: no leading or trailing
/// slash, no empty segments. Refuses `.`, `..`, backslashes and control
/// characters, so a path can never climb out of the root.
pub fn clean(path: &str) -> Result<String> {
    let mut parts = Vec::new();
    for segment in path.split('/') {
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." || segment.contains('\\') || segment.chars().any(char::is_control) {
            return Err(Error::InvalidPath(path.to_string()));
        }
        parts.push(segment);
    }
    Ok(parts.join("/"))
}

/// Joins already-clean path pieces.
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_slashes() {
        assert_eq!(clean("/a//b/").unwrap(), "a/b");
        assert_eq!(clean("").unwrap(), "");
        assert_eq!(clean("/").unwrap(), "");
    }

    #[test]
    fn refuses_escapes() {
        for bad in ["..", "a/../b", "./a", "a\\b", "a/\u{0}"] {
            assert!(clean(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn joins() {
        assert_eq!(join("", "a"), "a");
        assert_eq!(join("a", "b"), "a/b");
    }
}
