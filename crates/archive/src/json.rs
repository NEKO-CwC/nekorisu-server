//! Tolerant JSON readers: several spellings per field, numbers or RFC 3339
//! strings for times, missing fields are fine.

use serde_json::Value;

pub fn get<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(*k)).filter(|v| !v.is_null())
}

pub fn string(v: &Value, keys: &[&str]) -> Option<String> {
    get(v, keys).and_then(Value::as_str).map(str::to_owned)
}

pub fn uint(v: &Value, keys: &[&str]) -> Option<u64> {
    get(v, keys).and_then(Value::as_u64)
}

pub fn boolean(v: &Value, keys: &[&str]) -> bool {
    get(v, keys).and_then(Value::as_bool).unwrap_or(false)
}

/// Milliseconds since the epoch from a number (seconds or milliseconds) or an
/// RFC 3339 string.
pub fn time_ms(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => {
            let n = n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))?;
            Some(if n.abs() < 100_000_000_000 { n * 1000 } else { n })
        }
        Value::String(s) => time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
            .ok()
            .map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64),
        _ => None,
    }
}

pub fn time_field(v: &Value, keys: &[&str]) -> Option<i64> {
    get(v, keys).and_then(time_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_times_in_any_form() {
        assert_eq!(time_ms(&json!(1_794_700_800_000_i64)), Some(1_794_700_800_000));
        assert_eq!(time_ms(&json!(1_794_700_800)), Some(1_794_700_800_000));
        assert_eq!(time_ms(&json!("2026-11-15T00:00:00Z")), Some(1_794_700_800_000));
        assert_eq!(
            time_ms(&json!("2026-11-15T08:00:00.500+08:00")),
            Some(1_794_700_800_500)
        );
        assert_eq!(time_ms(&json!("garbage")), None);
        assert_eq!(time_ms(&json!(null)), None);
    }

    #[test]
    fn tries_aliases_in_order() {
        let v = json!({ "b": 2, "c": null });
        assert_eq!(uint(&v, &["a", "b"]), Some(2));
        assert_eq!(uint(&v, &["c"]), None);
    }
}
