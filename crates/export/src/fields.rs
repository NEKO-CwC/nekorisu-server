//! The fields of an export row and how they are written.

use serde_json::{Value, json};

use crate::ExportError;
use crate::turns::TurnRecord;

pub struct FieldInfo {
    pub name: &'static str,
    pub description: &'static str,
}

/// In the order they are written.
pub const FIELDS: &[FieldInfo] = &[
    FieldInfo {
        name: "session",
        description: "会话，写作 <agent>:<会话ID>",
    },
    FieldInfo {
        name: "agent",
        description: "Agent：claude-code 或 codex",
    },
    FieldInfo {
        name: "device",
        description: "上传这个会话的设备（名字，没有就是 ID）",
    },
    FieldInfo {
        name: "cwd",
        description: "会话的工作目录",
    },
    FieldInfo {
        name: "turn",
        description: "第几轮，从 1 数",
    },
    FieldInfo {
        name: "at",
        description: "这条用户原话的时间，RFC 3339（UTC）",
    },
    FieldInfo {
        name: "user",
        description: "用户原话，原样；CSV 里也不做任何转义改写",
    },
    FieldInfo {
        name: "prev_reply",
        description: "它回应的那条 Agent 回复：这条原话之前最后一条文字回复",
    },
    FieldInfo {
        name: "gap_since_reply",
        description: "距离上一条 Agent 回复的毫秒数",
    },
    FieldInfo {
        name: "gap_since_user",
        description: "距离上一条用户原话的毫秒数",
    },
    FieldInfo {
        name: "model",
        description: "这一轮用的模型，多个都列出（CSV 里用分号隔开）",
    },
    FieldInfo {
        name: "tokens",
        description: "这一轮所有模型请求的合计，含子 Agent：input、output、cache_read、cache_write（CSV 里展开成 tokens_input 等四列）",
    },
    FieldInfo {
        name: "requests",
        description: "模型请求次数，含子 Agent",
    },
    FieldInfo {
        name: "subagents",
        description: "这一轮里出现过的子 Agent 数",
    },
    FieldInfo {
        name: "reasoning_chars",
        description: "思考的字符数",
    },
    FieldInfo {
        name: "agent_version",
        description: "这一轮涉及的 Agent 程序版本（CSV 里用分号隔开）",
    },
    FieldInfo {
        name: "parse",
        description: "ok，或 partial：这一轮有东西没取到",
    },
];

/// Checks the requested names; none requested means all.
pub fn select(requested: &[String]) -> Result<Vec<&'static str>, ExportError> {
    if requested.is_empty() {
        return Ok(FIELDS.iter().map(|f| f.name).collect());
    }
    let mut out = Vec::new();
    for name in requested {
        let field = FIELDS
            .iter()
            .find(|f| f.name == name)
            .ok_or_else(|| ExportError::BadRequest(format!("unknown field: {name}")))?;
        if !out.contains(&field.name) {
            out.push(field.name);
        }
    }
    // Written in catalog order, whatever order was asked for.
    out.sort_by_key(|n| FIELDS.iter().position(|f| f.name == *n));
    Ok(out)
}

pub struct Row<'a> {
    pub session: &'a str,
    pub agent: &'a str,
    pub device: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub turn: &'a TurnRecord,
    /// `error` for a session that could not be read; the turn is then empty.
    pub error: Option<&'a str>,
}

fn rfc3339(ms: i64) -> Option<String> {
    time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .ok()?
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

fn value(row: &Row<'_>, field: &str) -> Value {
    let t = row.turn;
    match field {
        "session" => json!(row.session),
        "agent" => json!(row.agent),
        "device" => json!(row.device),
        "cwd" => json!(row.cwd),
        "turn" => json!(t.index),
        "at" => t.at_ms.and_then(rfc3339).map_or(Value::Null, Value::String),
        "user" => json!(t.user),
        "prev_reply" => json!(t.prev_reply),
        "gap_since_reply" => json!(t.gap_since_reply_ms),
        "gap_since_user" => json!(t.gap_since_user_ms),
        "model" => json!(t.models),
        "tokens" => json!({
            "input": t.tokens.input, "output": t.tokens.output,
            "cache_read": t.tokens.cache_read, "cache_write": t.tokens.cache_write,
        }),
        "requests" => json!(t.requests),
        "subagents" => json!(t.subagents),
        "reasoning_chars" => json!(t.reasoning_chars),
        "agent_version" => json!(t.versions),
        "parse" => json!(match (row.error, t.partial) {
            (Some(_), _) => "error",
            (None, true) => "partial",
            (None, false) => "ok",
        }),
        _ => Value::Null,
    }
}

/// One JSON object on one line, fields in catalog order.
pub fn jsonl(row: &Row<'_>, fields: &[&str]) -> String {
    let mut out = String::from("{");
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::to_string(f).expect("string"));
        out.push(':');
        out.push_str(&serde_json::to_string(&value(row, f)).expect("value"));
    }
    out.push_str("}\n");
    out
}

const TOKEN_COLUMNS: [&str; 4] = [
    "tokens_input",
    "tokens_output",
    "tokens_cache_read",
    "tokens_cache_write",
];

pub fn csv_header(fields: &[&str]) -> String {
    let mut cols: Vec<&str> = Vec::new();
    for f in fields {
        if *f == "tokens" {
            cols.extend(TOKEN_COLUMNS);
        } else {
            cols.push(f);
        }
    }
    let mut line = cols.join(",");
    line.push_str("\r\n");
    line
}

pub fn csv_row(row: &Row<'_>, fields: &[&str]) -> String {
    let mut cells: Vec<String> = Vec::new();
    for f in fields {
        let v = value(row, f);
        match (*f, &v) {
            ("tokens", Value::Object(o)) => {
                for k in ["input", "output", "cache_read", "cache_write"] {
                    cells.push(o.get(k).map(Value::to_string).unwrap_or_default());
                }
            }
            (_, Value::Null) => cells.push(String::new()),
            (_, Value::String(s)) => cells.push(s.clone()),
            (_, Value::Array(items)) => {
                let parts: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
                cells.push(parts.join(";"));
            }
            (_, other) => cells.push(other.to_string()),
        }
    }
    let mut line = cells.iter().map(|c| csv_cell(c)).collect::<Vec<_>>().join(",");
    line.push_str("\r\n");
    line
}

/// RFC 4180. Cells are quoted when they need it and otherwise left exactly as
/// they are: no spreadsheet-formula guarding, because the words of the user are
/// kept as said. Open a CSV from an unknown source with care.
fn csv_cell(cell: &str) -> String {
    if cell.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        cell.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Tokens;

    fn turn() -> TurnRecord {
        TurnRecord {
            index: 2,
            at_ms: Some(1_794_700_800_000),
            user: "say \"hi\", then\nbye".into(),
            prev_reply: None,
            gap_since_reply_ms: Some(5),
            models: vec!["a".into(), "b".into()],
            tokens: Tokens {
                input: 1,
                output: 2,
                cache_read: 3,
                cache_write: 4,
            },
            requests: 2,
            ..TurnRecord::default()
        }
    }

    fn row<'a>(t: &'a TurnRecord) -> Row<'a> {
        Row {
            session: "codex:t1",
            agent: "codex",
            device: Some("mac"),
            cwd: None,
            turn: t,
            error: None,
        }
    }

    #[test]
    fn selects_fields_in_catalog_order() {
        assert_eq!(select(&[]).unwrap().len(), FIELDS.len());
        let got = select(&["user".into(), "session".into(), "user".into()]).unwrap();
        assert_eq!(got, ["session", "user"]);
        assert!(select(&["nope".into()]).is_err());
    }

    #[test]
    fn writes_jsonl() {
        let t = turn();
        let line = jsonl(&row(&t), &["session", "at", "user", "tokens", "model", "cwd", "parse"]);
        let v: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(v["session"], "codex:t1");
        assert_eq!(v["at"], "2026-11-15T00:00:00Z");
        assert_eq!(v["user"], "say \"hi\", then\nbye");
        assert_eq!(v["tokens"]["cache_write"], 4);
        assert_eq!(v["model"], json!(["a", "b"]));
        assert_eq!(v["cwd"], Value::Null);
        assert_eq!(v["parse"], "ok");
        assert!(
            line.ends_with("}\n") && line.matches('\n').count() == 1,
            "one line per row"
        );
        // catalog order
        assert!(line.find("\"session\"").unwrap() < line.find("\"at\"").unwrap());
    }

    #[test]
    fn writes_csv() {
        let t = turn();
        let fields = ["turn", "user", "model", "tokens", "gap_since_user"];
        assert_eq!(
            csv_header(&fields),
            "turn,user,model,tokens_input,tokens_output,tokens_cache_read,tokens_cache_write,gap_since_user\r\n"
        );
        assert_eq!(
            csv_row(&row(&t), &fields),
            "2,\"say \"\"hi\"\", then\nbye\",a;b,1,2,3,4,\r\n"
        );
    }

    #[test]
    fn marks_partial_and_error_rows() {
        let mut t = turn();
        t.partial = true;
        assert!(jsonl(&row(&t), &["parse"]).contains("partial"));
        let mut r = row(&t);
        r.error = Some("boom");
        assert!(jsonl(&r, &["parse"]).contains("error"));
    }
}
