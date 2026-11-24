//! MCP over Streamable HTTP, read-only. Unlike the
//! client's `nekorisu mcp`, which reads one machine through its core, this one
//! reads every device's sessions as the server sees them.
//!
//! JSON-RPC 2.0 over POST, JSON replies, tools only: no server push, no
//! sessions. Hand-written, like the client's, because the surface is small.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use nekorisu_archive::{Kind, LinesQuery, SessionFilter};
use nekorisu_export::{ExportRequest, Format, Scope, display_text, export_stream, select_fields};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;

use crate::state::Shared;
use crate::util::{cut, parse_rfc3339, rfc3339};

const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const DEFAULT_READ_LIMIT: usize = 50;
const DEFAULT_FIELD_CHARS: usize = 2_000;
const MAX_EXPORT_ROWS: usize = 2_000;
const MAX_EXPORT_BYTES: usize = 512 * 1024;

fn reply(id: Value, result: Value) -> Response {
    json_response(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn error_reply(id: Value, code: i64, message: &str) -> Response {
    json_response(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
}

fn json_response(value: Value) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], value.to_string()).into_response()
}

fn authorized(s: &Shared, headers: &HeaderMap) -> Option<bool> {
    let expected = s.settings.mcp_token.as_deref().filter(|t| !t.is_empty())?;
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .unwrap_or_default();
    Some(bool::from(given.trim().as_bytes().ct_eq(expected.as_bytes())))
}

pub async fn handle(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    match authorized(&s, &headers) {
        None => return StatusCode::NOT_FOUND.into_response(), // MCP is off
        Some(false) => {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
                "a bearer token is required",
            )
                .into_response();
        }
        Some(true) => {}
    }
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return error_reply(Value::Null, -32700, "parse error");
    };
    if message.is_array() {
        return error_reply(Value::Null, -32600, "batches are not supported");
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return StatusCode::ACCEPTED.into_response(); // a response from the client: nothing to do
    };
    let Some(id) = message.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response(); // a notification
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => reply(id, initialize(&params)),
        "ping" => reply(id, json!({})),
        "tools/list" => reply(id, json!({ "tools": tools() })),
        "tools/call" => reply(id, call(&s, &params).await),
        other => error_reply(id, -32601, &format!("method not found: {other}")),
    }
}

fn initialize(params: &Value) -> Value {
    let asked = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    let version = if PROTOCOL_VERSIONS.contains(&asked) {
        asked
    } else {
        PROTOCOL_VERSIONS[0]
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "nekorisu-server", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Agent sessions (Claude Code, Codex) from all of the user's devices, kept byte-exact. Read-only. \
            list_sessions to find, get_session for versions and files, then read_lines with kinds=[\"user\"] for what the \
            person said, more only if needed. export gives one row per user message with timings and tokens."
    })
}

fn tools() -> Value {
    let session = json!({ "type": "string", "description": "<agent>:<id>, or just <id>" });
    let kinds = json!({ "type": "array", "items": { "type": "string", "enum": ["user", "assistant", "reasoning", "tool", "meta"] } });
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    json!([
        {
            "name": "list_sessions",
            "description": "Find sessions across devices and agents, newest first.",
            "inputSchema": { "type": "object", "properties": {
                "agent": { "type": "string", "enum": ["claude-code", "codex"] },
                "device": { "type": "string", "description": "device ID or name" },
                "query": { "type": "string", "description": "words in title, directory or id" },
                "limit": { "type": "integer" }, "include_children": { "type": "boolean" }
            } }
        },
        {
            "name": "get_session",
            "description": "Versions (more than one head is a fork), files and sub-sessions of a session.",
            "inputSchema": { "type": "object", "properties": { "session": session, "version": { "type": "string" } }, "required": ["session"] }
        },
        {
            "name": "read_lines",
            "description": "Read lines of a session by number or kind (user = what the person said). Text fields are cut to max_field_chars (default 2000; 0 = no cut).",
            "inputSchema": { "type": "object", "properties": {
                "session": session, "file": { "type": "string" }, "version": { "type": "string" },
                "from": { "type": "integer" }, "to": { "type": "integer" }, "kinds": kinds,
                "limit": { "type": "integer", "description": "default 50, at most 500" },
                "offset": { "type": "integer" }, "max_field_chars": { "type": "integer" }
            }, "required": ["session"] }
        },
        {
            "name": "list_devices",
            "description": "The user's devices, what they uploaded, and when they last reported.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "export",
            "description": "One JSON row per user message: the words, the reply they answer, gaps, model, tokens (sub-agents included). Choose fields to keep it small. Up to 200 rows by default.",
            "inputSchema": { "type": "object", "properties": {
                "fields": strings, "agent": { "type": "string" }, "device": { "type": "string" },
                "sessions": strings, "since": { "type": "string", "description": "RFC 3339" },
                "until": { "type": "string", "description": "RFC 3339" }, "limit": { "type": "integer" }
            } }
        }
    ])
}

async fn call(s: &Shared, params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let result = match name {
        "list_sessions" => list_sessions(s, &args).await,
        "get_session" => get_session(s, &args).await,
        "read_lines" => read_lines(s, &args).await,
        "list_devices" => list_devices(s).await,
        "export" => export(s, &args).await,
        other => Err(format!("unknown tool: {other}")),
    };
    match result {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
        Err(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": true }),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).filter(|v| !v.is_empty())
}

fn arg_num(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn failure(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// `<agent>:<id>`, or a bare id that belongs to exactly one session.
async fn resolve(s: &Shared, text: &str) -> Result<(String, String), String> {
    if let Some((agent, id)) = text.split_once(':')
        && !agent.is_empty()
        && !id.is_empty()
    {
        return Ok((agent.to_string(), id.to_string()));
    }
    let index = s.archive.index().await.map_err(failure)?;
    let mut found = index.find_id(text);
    match found.len() {
        0 => Err(format!("no session with id {text}")),
        1 => Ok(found.remove(0)),
        _ => Err(format!("{text} is ambiguous; write <agent>:<id>")),
    }
}

async fn device_id(s: &Shared, wanted: &str) -> Result<String, String> {
    let index = s.archive.index().await.map_err(failure)?;
    index
        .devices
        .iter()
        .find(|d| d.id == wanted || d.name.as_deref() == Some(wanted))
        .map(|d| d.id.clone())
        .ok_or_else(|| format!("no device {wanted}"))
}

async fn list_sessions(s: &Shared, args: &Value) -> Result<String, String> {
    let device = match arg_str(args, "device") {
        Some(d) => Some(device_id(s, d).await?),
        None => None,
    };
    let filter = SessionFilter {
        agent: arg_str(args, "agent").map(str::to_owned),
        device,
        q: arg_str(args, "query").map(str::to_owned),
        include_children: args.get("include_children").and_then(Value::as_bool).unwrap_or(false),
        limit: (arg_num(args, "limit").unwrap_or(30) as usize).clamp(1, 200),
        offset: 0,
    };
    let (total, items) = s.archive.sessions(&filter).await.map_err(failure)?;
    let mut text = format!("{} of {total} sessions\n", items.len());
    for i in items {
        let devices: Vec<String> = i
            .devices
            .iter()
            .map(|d| d.name.clone().unwrap_or_else(|| d.id.clone()))
            .collect();
        text.push_str(
            &json!({
                "session": i.session_ref, "title": i.title, "cwd": i.cwd, "devices": devices,
                "last_activity": i.last_ts_ms.map(rfc3339), "bytes": i.bytes, "versions": i.versions,
                "heads": i.heads, "parent": i.parent, "children": i.children,
            })
            .to_string(),
        );
        text.push('\n');
    }
    Ok(text)
}

async fn get_session(s: &Shared, args: &Value) -> Result<String, String> {
    let (agent, id) = resolve(s, arg_str(args, "session").ok_or("session is required")?).await?;
    let d = s
        .archive
        .session(&agent, &id, arg_str(args, "version"))
        .await
        .map_err(failure)?;
    let versions: Vec<Value> = d
        .versions
        .iter()
        .take(20)
        .map(|v| {
            json!({
                "id": v.id, "parent": v.parent, "device": v.device.name.clone().unwrap_or_else(|| v.device.id.clone()),
                "uploaded": rfc3339(v.created_ms), "bytes": v.bytes, "head": v.is_head, "rewrite": v.rewrite,
            })
        })
        .collect();
    Ok(json!({
        "session": d.summary.session_ref, "title": d.summary.title, "cwd": d.summary.cwd,
        "agent_version": d.summary.agent_version, "selected_version": d.selected_version,
        "heads": d.summary.heads, "versions": versions,
        "files": d.files.iter().map(|f| json!({ "path": f.path, "role": f.role, "bytes": f.bytes, "lines": f.lines })).collect::<Vec<_>>(),
        "children": d.children.iter().map(|c| c.session_ref.clone()).collect::<Vec<_>>(),
        "fork_from": d.fork_from.map(|f| json!({ "session": f.session, "byte": f.byte })),
        "digests": d.digests.iter().map(|g| g.id.clone()).collect::<Vec<_>>(),
    })
    .to_string())
}

async fn read_lines(s: &Shared, args: &Value) -> Result<String, String> {
    let (agent, id) = resolve(s, arg_str(args, "session").ok_or("session is required")?).await?;
    let mut kinds = Vec::new();
    for k in args
        .get("kinds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        kinds.push(Kind::parse(k).ok_or_else(|| format!("unknown kind: {k}"))?);
    }
    let query = LinesQuery {
        file: arg_str(args, "file").map(str::to_owned),
        from: arg_num(args, "from"),
        to: arg_num(args, "to"),
        kinds,
        select: None,
        limit: (arg_num(args, "limit").unwrap_or(DEFAULT_READ_LIMIT as u64) as usize).clamp(1, 500),
        offset: arg_num(args, "offset").unwrap_or(0) as usize,
    };
    let max_chars = arg_num(args, "max_field_chars").map_or(DEFAULT_FIELD_CHARS, |n| n as usize);
    let page = s
        .archive
        .lines(&agent, &id, arg_str(args, "version"), &query)
        .await
        .map_err(failure)?;
    let mut text = format!(
        "{} of {} matching lines ({} in the file){}; file: {}; version: {}\n",
        page.items.len(),
        page.matched,
        page.total_lines,
        if page.truncated {
            ", more match: raise limit or use offset"
        } else {
            ""
        },
        page.file,
        page.version
    );
    for r in &page.items {
        let (body, _) = cut(&display_text(&agent, r.kind, &r.raw_type, &r.raw), max_chars);
        text.push_str(&format!("L{} {} {}\n", r.line, r.kind.as_str(), body));
    }
    Ok(text)
}

async fn list_devices(s: &Shared) -> Result<String, String> {
    let stats = s.archive.devices().await.map_err(failure)?;
    let states: std::collections::HashMap<_, _> = s.store.device_states().await.map_err(failure)?.into_iter().collect();
    let mut text = String::new();
    for d in stats {
        text.push_str(
            &json!({
                "id": d.id, "name": d.name, "sessions": d.sessions, "bytes": d.bytes,
                "last_upload": d.last_upload_ms.map(rfc3339),
                "last_report": states.get(&d.id).map(|st| rfc3339(st.received_ms)),
            })
            .to_string(),
        );
        text.push('\n');
    }
    Ok(if text.is_empty() {
        "no devices".to_string()
    } else {
        text
    })
}

async fn export(s: &Shared, args: &Value) -> Result<String, String> {
    let strings = |key: &str| -> Vec<String> {
        args.get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default()
    };
    let time = |key: &str| -> Result<Option<i64>, String> {
        arg_str(args, key)
            .map(|t| parse_rfc3339(t).ok_or_else(|| format!("{key} must be RFC 3339, like 2026-11-15T00:00:00Z")))
            .transpose()
    };
    let device = match arg_str(args, "device") {
        Some(d) => Some(device_id(s, d).await?),
        None => None,
    };
    let limit = (arg_num(args, "limit").unwrap_or(200) as usize).clamp(1, MAX_EXPORT_ROWS);
    let request = ExportRequest {
        scope: Scope {
            agent: arg_str(args, "agent").map(str::to_owned),
            device,
            sessions: strings("sessions"),
            since_ms: time("since")?,
            until_ms: time("until")?,
        },
        fields: select_fields(&strings("fields")).map_err(failure)?,
        format: Format::Jsonl,
        max_rows: Some(limit),
    };
    let mut stream = Box::pin(export_stream(s.archive.clone(), request));
    let mut text = String::new();
    let mut rows = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(failure)?;
        if text.len() + chunk.len() > MAX_EXPORT_BYTES {
            text.push_str(
                "(stopped: the answer reached its size limit; narrow with fields, sessions or since/until)\n",
            );
            return Ok(text);
        }
        text.push_str(&String::from_utf8_lossy(&chunk));
        rows += 1;
    }
    if rows == 0 {
        return Ok("no rows".to_string());
    }
    if rows >= limit {
        text.push_str(&format!(
            "(stopped at {limit} rows; use since/until or sessions to see others)\n"
        ));
    }
    Ok(text)
}
