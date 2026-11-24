use serde_json::Value;

use crate::json;

/// One record of the operation log.
#[derive(Clone, Debug)]
pub struct LogRecord {
    pub at_ms: i64,
    pub seq: u64,
    /// The device whose directory the record was found in.
    pub device_id: String,
    pub op: String,
    pub sessions: Vec<String>,
    pub tags: Vec<String>,
    pub note: Option<String>,
    pub bytes: Option<u64>,
}

/// Parses a JSONL file. Lines that are not records are counted, not fatal.
pub(crate) fn parse(bytes: &[u8], device_dir: &str) -> (Vec<LogRecord>, u64) {
    let mut records = Vec::new();
    let mut skipped = 0;
    for line in bytes.split(|b| *b == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<Value>(line) {
            Ok(v) if v.is_object() => records.push(record(&v, device_dir)),
            _ => skipped += 1,
        }
    }
    (records, skipped)
}

fn record(v: &Value, device_dir: &str) -> LogRecord {
    let strings = |keys: &[&str]| -> Vec<String> {
        json::get(v, keys)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| match x {
                        Value::String(s) => Some(s.clone()),
                        other => {
                            let agent = json::string(other, &["agent"])?;
                            let id = json::string(other, &["id"])?;
                            Some(format!("{agent}:{id}"))
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    // `detail` as an object, or `detail_json` as text holding one.
    let detail: Option<Value> = json::get(v, &["detail"])
        .cloned()
        .or_else(|| json::string(v, &["detail_json"]).and_then(|t| serde_json::from_str(&t).ok()));
    LogRecord {
        at_ms: json::time_field(v, &["at_ms", "at", "ts", "time"]).unwrap_or(0),
        seq: json::uint(v, &["seq"]).unwrap_or(0),
        device_id: json::string(v, &["device"]).unwrap_or_else(|| device_dir.to_string()),
        op: json::string(v, &["op"]).unwrap_or_default(),
        sessions: strings(&["sessions"]),
        tags: strings(&["tags"]),
        note: json::string(v, &["note"]).filter(|n| !n.is_empty()),
        bytes: detail.as_ref().and_then(|d| json::uint(d, &["bytes", "size"])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records_and_counts_bad_lines() {
        let text = b"{\"seq\":1,\"at_ms\":5000,\"op\":\"archive\",\"sessions\":[\"codex:t1\",{\"agent\":\"claude-code\",\"id\":\"s\"}],\"detail_json\":\"{\\\"bytes\\\":42}\"}\n\nnot json\n[1]\n";
        let (records, skipped) = parse(text, "d1");
        assert_eq!(records.len(), 1);
        assert_eq!(skipped, 2);
        assert_eq!(records[0].device_id, "d1");
        assert_eq!(records[0].sessions, ["codex:t1", "claude-code:s"]);
        assert_eq!(records[0].bytes, Some(42));
    }
}
