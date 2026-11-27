//! Path maps edited in the panel. The server stores them as text and hands
//! them to devices; only devices read them as paths. The checks here are about
//! size and about two rules that would leave a device guessing.

use crate::doc::PathMapDoc;

pub const MAX_PATH_MAPS: usize = 500;
/// Bytes, for `from` and `to` alike.
pub const MAX_PATH_TEXT: usize = 4096;
const MAX_DEVICES: usize = 200;

/// Why a table of path maps was not accepted.
#[derive(Debug, PartialEq, Eq)]
pub struct PathMapError(pub String);

impl std::fmt::Display for PathMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PathMapError {}

pub fn check_path_maps(maps: &[PathMapDoc]) -> Result<(), PathMapError> {
    let bad = |why: String| Err(PathMapError(why));
    if maps.len() > MAX_PATH_MAPS {
        return bad(format!("more than {MAX_PATH_MAPS} path maps"));
    }
    for (i, m) in maps.iter().enumerate() {
        let n = i + 1;
        if m.from.trim().is_empty() {
            return bad(format!("path map {n}: `from` is empty"));
        }
        if m.from.len() > MAX_PATH_TEXT || m.to.len() > MAX_PATH_TEXT {
            return bad(format!("path map {n}: over {MAX_PATH_TEXT} bytes"));
        }
        if m.from.chars().chain(m.to.chars()).any(char::is_control) {
            return bad(format!("path map {n}: control characters"));
        }
        if m.devices.len() > MAX_DEVICES {
            return bad(format!("path map {n}: more than {MAX_DEVICES} devices"));
        }
        for d in &m.devices {
            let plain = !d.is_empty()
                && d.len() <= 128
                && d.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
            if !plain {
                return bad(format!("path map {n}: {d:?} is not a device ID"));
            }
        }
        // The same `from` twice for one device: which one would it take? A rule
        // for every device and one for a few may share a `from`: the few take
        // their own.
        for (j, other) in maps[..i].iter().enumerate() {
            let overlap = if m.devices.is_empty() || other.devices.is_empty() {
                m.devices.is_empty() && other.devices.is_empty()
            } else {
                m.devices.iter().any(|d| other.devices.contains(d))
            };
            if other.from == m.from && overlap {
                return bad(format!(
                    "path maps {} and {n} have the same `from` for the same device",
                    j + 1
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(from: &str, to: &str, devices: &[&str]) -> PathMapDoc {
        PathMapDoc {
            from: from.into(),
            to: to.into(),
            devices: devices.iter().map(|d| d.to_string()).collect(),
        }
    }

    #[test]
    fn accepts_plain_tables() {
        assert!(check_path_maps(&[]).is_ok());
        assert!(
            check_path_maps(&[
                map("D:\\work", "/Users/me/work", &[]),
                map("D:\\work", "/home/me/work", &["dev-1"]),
                map("D:\\work", "", &["dev-2"]),
                map("/home/neko/src", "C:\\src", &["dev-3", "dev-4"]),
            ])
            .is_ok()
        );
    }

    #[test]
    fn refuses_what_a_device_could_not_follow() {
        let err = |maps: &[PathMapDoc]| check_path_maps(maps).unwrap_err().0;
        assert!(err(&[map(" ", "/x", &[])]).contains("empty"));
        assert!(err(&[map("/a", "/x", &["../d"])]).contains("device ID"));
        assert!(err(&[map("/a", "/x\n", &[])]).contains("control"));
        assert!(err(&[map(&"a".repeat(MAX_PATH_TEXT + 1), "/x", &[])]).contains("bytes"));
        assert!(err(&[map("/a", "/x", &[]), map("/a", "/y", &[])]).contains("same `from`"));
        assert!(err(&[map("/a", "/x", &["d1", "d2"]), map("/a", "/y", &["d2"])]).contains("same `from`"));
        let many: Vec<PathMapDoc> = (0..=MAX_PATH_MAPS).map(|i| map(&format!("/p{i}"), "/x", &[])).collect();
        assert!(err(&many).contains("more than"));
    }
}
