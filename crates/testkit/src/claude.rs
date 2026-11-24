//! Claude Code transcript lines, invented.

use serde_json::json;

use crate::{Line, Usage, rfc3339};

fn usage_json(u: Usage) -> serde_json::Value {
    json!({
        "input_tokens": u.input, "output_tokens": u.output,
        "cache_read_input_tokens": u.cache_read, "cache_creation_input_tokens": u.cache_write,
    })
}

pub fn user(ts_ms: i64, text: &str) -> Line {
    let json = json!({
        "type": "user", "timestamp": rfc3339(ts_ms), "version": "2.1.322", "cwd": "/work/demo",
        "message": { "role": "user", "content": text },
    });
    Line {
        kind: "user",
        raw_type: "user".into(),
        ts_ms,
        json: json.to_string(),
    }
}

pub fn assistant_text(ts_ms: i64, message_id: &str, model: &str, usage: Usage, text: &str) -> Line {
    assistant(
        ts_ms,
        message_id,
        model,
        usage,
        json!([{ "type": "text", "text": text }]),
        "assistant",
    )
}

pub fn assistant_thinking(ts_ms: i64, message_id: &str, model: &str, usage: Usage, text: &str) -> Line {
    assistant(
        ts_ms,
        message_id,
        model,
        usage,
        json!([{ "type": "thinking", "thinking": text }]),
        "reasoning",
    )
}

pub fn assistant_tool(ts_ms: i64, message_id: &str, model: &str, usage: Usage, tool: &str) -> Line {
    assistant(
        ts_ms,
        message_id,
        model,
        usage,
        json!([{ "type": "tool_use", "id": "toolu_1", "name": tool, "input": {} }]),
        "tool",
    )
}

fn assistant(
    ts_ms: i64,
    message_id: &str,
    model: &str,
    usage: Usage,
    content: serde_json::Value,
    kind: &'static str,
) -> Line {
    let json = json!({
        "type": "assistant", "timestamp": rfc3339(ts_ms), "version": "2.1.322",
        "message": { "id": message_id, "role": "assistant", "model": model, "content": content, "usage": usage_json(usage) },
    });
    Line {
        kind,
        raw_type: "assistant".into(),
        ts_ms,
        json: json.to_string(),
    }
}

pub fn tool_result(ts_ms: i64, text: &str) -> Line {
    let json = json!({
        "type": "user", "timestamp": rfc3339(ts_ms), "version": "2.1.322",
        "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "toolu_1", "content": text }] },
    });
    Line {
        kind: "tool",
        raw_type: "user".into(),
        ts_ms,
        json: json.to_string(),
    }
}

/// The first record of a sub-agent transcript: the main agent's instructions, not the person's words.
pub fn sidechain_user(ts_ms: i64, text: &str) -> Line {
    let json = json!({
        "type": "user", "timestamp": rfc3339(ts_ms), "version": "2.1.322", "isSidechain": true,
        "message": { "role": "user", "content": text },
    });
    Line {
        kind: "meta",
        raw_type: "user".into(),
        ts_ms,
        json: json.to_string(),
    }
}

pub fn title(text: &str) -> Line {
    let json = json!({ "type": "custom-title", "customTitle": text });
    Line {
        kind: "meta",
        raw_type: "custom-title".into(),
        ts_ms: 0,
        json: json.to_string(),
    }
}
