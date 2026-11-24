//! The line map of one segment: where each line starts, how long it is, what
//! kind it is. No content.
//!
//! Offsets are relative to the start of the segment: the file is named by the
//! segment's SHA-256, so the same bytes at another position share it.

use serde_json::Value;

use crate::json;
use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    User,
    Assistant,
    Reasoning,
    Tool,
    Meta,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::User, Kind::Assistant, Kind::Reasoning, Kind::Tool, Kind::Meta];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Assistant => "assistant",
            Kind::Reasoning => "reasoning",
            Kind::Tool => "tool",
            Kind::Meta => "meta",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineEntry {
    /// Relative to the segment start.
    pub offset: u64,
    /// Without the line break.
    pub len: u64,
    pub kind: Kind,
    /// Index into `LineMap::raw_types`.
    pub raw_type: u16,
    /// 0 when the line has no timestamp.
    pub ts_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineMap {
    pub raw_types: Vec<String>,
    pub lines: Vec<LineEntry>,
}

impl LineMap {
    /// Accepts the canonical shape,
    /// `{"raw_types": [..], "lines": [[offset, len, kind, raw_type, ts_ms], ..]}`,
    /// and rows written as objects with the same keys.
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let value: Value = serde_json::from_slice(bytes).map_err(|e| Error::format("line map", e.to_string()))?;
        let mut map = LineMap {
            raw_types: json::get(&value, &["raw_types"])
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default(),
            lines: Vec::new(),
        };
        let rows = json::get(&value, &["lines"])
            .and_then(Value::as_array)
            .ok_or_else(|| Error::format("line map", "no `lines`"))?;
        for row in rows {
            let entry = match row {
                Value::Array(a) => LineEntry {
                    offset: a.first().and_then(Value::as_u64).unwrap_or(0),
                    len: a.get(1).and_then(Value::as_u64).unwrap_or(0),
                    kind: a.get(2).map(kind_of).unwrap_or(Kind::Meta),
                    raw_type: raw_type_of(a.get(3), &mut map.raw_types),
                    ts_ms: a.get(4).and_then(json::time_ms).unwrap_or(0),
                },
                Value::Object(_) => LineEntry {
                    offset: json::uint(row, &["offset", "o"]).unwrap_or(0),
                    len: json::uint(row, &["len", "length", "l"]).unwrap_or(0),
                    kind: json::get(row, &["kind", "k"]).map(kind_of).unwrap_or(Kind::Meta),
                    raw_type: raw_type_of(json::get(row, &["raw_type", "r"]), &mut map.raw_types),
                    ts_ms: json::time_field(row, &["ts_ms", "ts", "t"]).unwrap_or(0),
                },
                _ => return Err(Error::format("line map", "a row is neither an array nor an object")),
            };
            map.lines.push(entry);
        }
        Ok(map)
    }

    pub fn raw_type(&self, entry: &LineEntry) -> &str {
        self.raw_types
            .get(entry.raw_type as usize)
            .map(String::as_str)
            .unwrap_or("")
    }
}

/// A kind written as its name, or as an index into `Kind::ALL`; unknown is `meta`.
fn kind_of(v: &Value) -> Kind {
    match v {
        Value::String(s) => Kind::parse(s).unwrap_or(Kind::Meta),
        Value::Number(n) => n
            .as_u64()
            .and_then(|i| Kind::ALL.get(i as usize).copied())
            .unwrap_or(Kind::Meta),
        _ => Kind::Meta,
    }
}

/// A raw type given as an index into `raw_types`, or as a name that is added to it.
fn raw_type_of(v: Option<&Value>, raw_types: &mut Vec<String>) -> u16 {
    match v {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0).min(u16::MAX as u64) as u16,
        Some(Value::String(s)) => match raw_types.iter().position(|t| t == s) {
            Some(i) => i as u16,
            None if raw_types.len() < u16::MAX as usize => {
                raw_types.push(s.clone());
                (raw_types.len() - 1) as u16
            }
            None => 0,
        },
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_array_rows() {
        let text = br#"{"raw_types":["user","assistant"],"lines":[[0,10,"user",0,1000],[11,20,"assistant",1,0]]}"#;
        let map = LineMap::from_json(text).unwrap();
        assert_eq!(map.lines.len(), 2);
        assert_eq!(map.lines[1].offset, 11);
        assert_eq!(map.lines[1].kind, Kind::Assistant);
        assert_eq!(map.raw_type(&map.lines[1]), "assistant");
        assert_eq!(map.lines[0].ts_ms, 1_000_000);
    }

    #[test]
    fn reads_object_rows_and_unknown_kinds() {
        let text = br#"{"lines":[{"offset":0,"len":5,"kind":"weird","raw_type":"x/y","ts":"2026-11-15T00:00:00Z"}]}"#;
        let map = LineMap::from_json(text).unwrap();
        assert_eq!(map.lines[0].kind, Kind::Meta);
        assert_eq!(map.raw_type(&map.lines[0]), "x/y");
        assert_eq!(map.lines[0].ts_ms, 1_794_700_800_000);
    }

    #[test]
    fn refuses_nonsense() {
        assert!(LineMap::from_json(b"[]").is_err());
        assert!(LineMap::from_json(b"{\"lines\":[1]}").is_err());
        assert!(LineMap::from_json(b"nope").is_err());
    }
}
