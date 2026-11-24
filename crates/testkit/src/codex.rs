//! Codex rollout lines, invented. The shapes follow what the client's line map
//! recognizes (legacy `event_msg` records and `item_completed` items).

use serde_json::json;

use crate::{Line, rfc3339};

pub fn session_meta(ts_ms: i64, thread_id: &str, cli_version: &str, cwd: &str, parent_thread: Option<&str>) -> Line {
    let mut payload = json!({ "id": thread_id, "cli_version": cli_version, "cwd": cwd, "source": "cli" });
    if let Some(parent) = parent_thread {
        payload["parent_thread_id"] = json!(parent);
    }
    let json = json!({ "timestamp": rfc3339(ts_ms), "type": "session_meta", "payload": payload });
    Line {
        kind: "meta",
        raw_type: "session_meta".into(),
        ts_ms,
        json: json.to_string(),
    }
}

pub fn turn_context(ts_ms: i64, model: &str) -> Line {
    let json = json!({ "timestamp": rfc3339(ts_ms), "type": "turn_context", "payload": { "model": model } });
    Line {
        kind: "meta",
        raw_type: "turn_context".into(),
        ts_ms,
        json: json.to_string(),
    }
}

pub fn user(ts_ms: i64, text: &str) -> Line {
    event(
        ts_ms,
        "user",
        "user_message",
        json!({ "type": "user_message", "message": text }),
    )
}

pub fn agent_message(ts_ms: i64, text: &str) -> Line {
    event(
        ts_ms,
        "assistant",
        "agent_message",
        json!({ "type": "agent_message", "message": text }),
    )
}

pub fn reasoning(ts_ms: i64, text: &str) -> Line {
    event(
        ts_ms,
        "reasoning",
        "agent_reasoning",
        json!({ "type": "agent_reasoning", "text": text }),
    )
}

pub fn command(ts_ms: i64) -> Line {
    event(
        ts_ms,
        "tool",
        "exec_command_end",
        json!({ "type": "exec_command_end", "exit_code": 0 }),
    )
}

/// Cumulative token totals, as Codex reports them: `input` includes `cached`.
pub fn token_count(ts_ms: i64, input: u64, cached: u64, output: u64, reasoning: u64) -> Line {
    let usage = json!({
        "input_tokens": input, "cached_input_tokens": cached, "output_tokens": output,
        "reasoning_output_tokens": reasoning, "total_tokens": input + output,
    });
    event(
        ts_ms,
        "meta",
        "token_count",
        json!({ "type": "token_count", "info": { "total_token_usage": usage } }),
    )
}

fn event(ts_ms: i64, kind: &'static str, name: &str, payload: serde_json::Value) -> Line {
    let json = json!({ "timestamp": rfc3339(ts_ms), "type": "event_msg", "payload": payload });
    Line {
        kind,
        raw_type: format!("event_msg/{name}"),
        ts_ms,
        json: json.to_string(),
    }
}
