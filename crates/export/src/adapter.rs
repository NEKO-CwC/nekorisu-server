//! What each agent's records say: the text of a message, the model and token
//! counts of a request. The only part of the server that knows the agents'
//! formats.
//!
//! There is one set of rules per agent. Both agents change formats between
//! versions and neither has a format version, only a program version
//!, so when a version needs
//! its own rules this is where they go. Anything a rule cannot find is `None`,
//! and the caller marks the row `partial`; nothing here fails.
//!
//! Not verified against real sessions yet: this environment never reads
//! session content. The tests use invented records in the shapes the client's
//! line map recognizes.

use nekorisu_archive::Kind;
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    Other,
}

pub fn agent_kind(name: &str) -> AgentKind {
    match name {
        "claude-code" => AgentKind::ClaudeCode,
        "codex" => AgentKind::Codex,
        _ => AgentKind::Other,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    pub fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }

    pub fn max_with(&mut self, other: &Tokens) {
        self.input = self.input.max(other.input);
        self.output = self.output.max(other.output);
        self.cache_read = self.cache_read.max(other.cache_read);
        self.cache_write = self.cache_write.max(other.cache_write);
    }

    pub fn is_zero(&self) -> bool {
        *self == Tokens::default()
    }
}

fn parse(raw: &[u8]) -> Option<Value> {
    serde_json::from_slice(raw).ok()
}

// ---- Claude Code ----

/// One model request as Claude Code records it. A request is written as
/// several records, one per content block, that share `message_id`.
#[derive(Clone, Debug, Default)]
pub struct ClaudeRequest {
    pub message_id: Option<String>,
    pub model: Option<String>,
    pub usage: Option<Tokens>,
    pub version: Option<String>,
    /// Characters of thinking in this record.
    pub reasoning_chars: u64,
}

pub fn claude_request(raw: &[u8]) -> Option<ClaudeRequest> {
    let v = parse(raw)?;
    let message = v.get("message")?;
    let usage = message.get("usage").map(|u| {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        Tokens {
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
        }
    });
    let reasoning_chars = message
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("thinking"))
                .filter_map(|b| b.get("thinking").and_then(Value::as_str))
                .map(|t| t.chars().count() as u64)
                .sum()
        })
        .unwrap_or(0);
    Some(ClaudeRequest {
        message_id: message.get("id").and_then(Value::as_str).map(str::to_owned),
        model: message.get("model").and_then(Value::as_str).map(str::to_owned),
        usage,
        version: v.get("version").and_then(Value::as_str).map(str::to_owned),
        reasoning_chars,
    })
}

/// Text blocks of a Claude Code message; a plain string content is one block.
fn claude_text(v: &Value, block_type: &str, field: &str) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    match content {
        Value::String(s) if block_type == "text" => Some(s.clone()),
        Value::Array(blocks) => {
            let parts: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some(block_type))
                .filter_map(|b| b.get(field).and_then(Value::as_str))
                .collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        _ => None,
    }
}

pub fn claude_version(raw: &[u8]) -> Option<String> {
    parse(raw)?.get("version")?.as_str().map(str::to_owned)
}

// ---- Codex ----

/// Cumulative token totals as a `token_count` event reports them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodexTotals {
    /// Includes `cached`.
    pub input: u64,
    pub cached: u64,
    pub output: u64,
}

impl CodexTotals {
    /// What was added since `earlier`. A total that went down means the counter
    /// restarted, so the new total is all new.
    pub fn since(&self, earlier: Option<&CodexTotals>) -> CodexTotals {
        match earlier {
            Some(e) if self.input >= e.input && self.output >= e.output && self.cached >= e.cached => CodexTotals {
                input: self.input - e.input,
                cached: self.cached - e.cached,
                output: self.output - e.output,
            },
            _ => *self,
        }
    }

    /// Both agents' numbers on one footing: `input` without the cached part.
    pub fn tokens(&self) -> Tokens {
        Tokens {
            input: self.input.saturating_sub(self.cached),
            output: self.output,
            cache_read: self.cached,
            cache_write: 0,
        }
    }
}

pub fn codex_token_total(raw: &[u8]) -> Option<CodexTotals> {
    let v = parse(raw)?;
    let usage = v.get("payload")?.get("info")?.get("total_token_usage")?;
    let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
    Some(CodexTotals {
        input: n("input_tokens"),
        cached: n("cached_input_tokens"),
        output: n("output_tokens"),
    })
}

/// The model named in a `turn_context` record.
pub fn codex_model(raw: &[u8]) -> Option<String> {
    parse(raw)?.get("payload")?.get("model")?.as_str().map(str::to_owned)
}

/// The program version in a `session_meta` record.
pub fn codex_version(raw: &[u8]) -> Option<String> {
    parse(raw)?
        .get("payload")?
        .get("cli_version")?
        .as_str()
        .map(str::to_owned)
}

/// The text of a Codex user message, agent message or reasoning record, from
/// the legacy `event_msg` shape or the `item_completed` shape.
pub fn codex_text(raw: &[u8]) -> Option<String> {
    let v = parse(raw)?;
    let payload = v.get("payload")?;
    for key in ["message", "text"] {
        if let Some(s) = payload.get(key).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    let item = payload.get("item")?;
    for key in ["message", "text"] {
        if let Some(s) = item.get(key).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    let parts: Vec<&str> = item
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|c| c.get("text").and_then(Value::as_str))
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

// ---- Both ----

/// The words of a user or assistant line. `None` when the line has none or its
/// shape is not one we know.
pub fn message_text(agent: AgentKind, kind: Kind, raw: &[u8]) -> Option<String> {
    match (agent, kind) {
        (AgentKind::ClaudeCode, Kind::User) => claude_text(&parse(raw)?, "text", "text"),
        (AgentKind::ClaudeCode, Kind::Assistant) => claude_text(&parse(raw)?, "text", "text"),
        (AgentKind::ClaudeCode, Kind::Reasoning) => claude_text(&parse(raw)?, "thinking", "thinking"),
        (AgentKind::Codex, Kind::User | Kind::Assistant | Kind::Reasoning) => codex_text(raw),
        _ => None,
    }
}

/// One line of readable text for the panel: the words for messages, a short
/// description for everything else.
pub fn display_text(agent: &str, kind: Kind, raw_type: &str, raw: &[u8]) -> String {
    let agent = agent_kind(agent);
    if let Some(text) = message_text(agent, kind, raw) {
        return text;
    }
    let fallback = || format!("({raw_type})");
    match (agent, kind) {
        (AgentKind::ClaudeCode, Kind::Tool) => {
            let Some(v) = parse(raw) else { return fallback() };
            let blocks = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut parts = Vec::new();
            for b in &blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("tool_use") => {
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("?");
                        parts.push(format!("tool call: {name}"));
                    }
                    Some("tool_result") => match b.get("content") {
                        Some(Value::String(s)) => parts.push(format!("tool result: {s}")),
                        _ => parts.push("tool result".to_string()),
                    },
                    _ => {}
                }
            }
            if parts.is_empty() { fallback() } else { parts.join("\n") }
        }
        _ => fallback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nekorisu_testkit::{Usage, claude, codex};

    #[test]
    fn claude_requests_carry_id_model_usage_and_version() {
        let line = claude::assistant_text(
            1,
            "msg_1",
            "model-a",
            Usage {
                input: 10,
                output: 20,
                cache_read: 30,
                cache_write: 40,
            },
            "hi",
        );
        let r = claude_request(line.json.as_bytes()).unwrap();
        assert_eq!(r.message_id.as_deref(), Some("msg_1"));
        assert_eq!(r.model.as_deref(), Some("model-a"));
        assert_eq!(
            r.usage,
            Some(Tokens {
                input: 10,
                output: 20,
                cache_read: 30,
                cache_write: 40
            })
        );
        assert_eq!(r.version.as_deref(), Some("2.1.322"));
        assert_eq!(r.reasoning_chars, 0);

        let thinking = claude::assistant_thinking(1, "msg_1", "model-a", Usage::default(), "héllo");
        assert_eq!(claude_request(thinking.json.as_bytes()).unwrap().reasoning_chars, 5);
    }

    #[test]
    fn extracts_message_text() {
        let u = claude::user(1, "what is 2+2");
        assert_eq!(
            message_text(AgentKind::ClaudeCode, Kind::User, u.json.as_bytes()).as_deref(),
            Some("what is 2+2")
        );
        let a = claude::assistant_text(2, "m", "x", Usage::default(), "four");
        assert_eq!(
            message_text(AgentKind::ClaudeCode, Kind::Assistant, a.json.as_bytes()).as_deref(),
            Some("four")
        );
        let t = claude::assistant_thinking(2, "m", "x", Usage::default(), "hmm");
        assert_eq!(
            message_text(AgentKind::ClaudeCode, Kind::Reasoning, t.json.as_bytes()).as_deref(),
            Some("hmm")
        );

        let c = codex::user(1, "hello");
        assert_eq!(
            message_text(AgentKind::Codex, Kind::User, c.json.as_bytes()).as_deref(),
            Some("hello")
        );
        let item = br#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}}"#;
        assert_eq!(
            message_text(AgentKind::Codex, Kind::User, item).as_deref(),
            Some("a\nb")
        );
        assert_eq!(message_text(AgentKind::Codex, Kind::User, b"not json"), None);
    }

    #[test]
    fn codex_totals_and_deltas() {
        let a = codex::token_count(1, 1000, 400, 50, 10);
        let b = codex::token_count(2, 1800, 900, 120, 20);
        let ta = codex_token_total(a.json.as_bytes()).unwrap();
        let tb = codex_token_total(b.json.as_bytes()).unwrap();
        assert_eq!(
            ta,
            CodexTotals {
                input: 1000,
                cached: 400,
                output: 50
            }
        );
        let delta = tb.since(Some(&ta));
        assert_eq!(
            delta,
            CodexTotals {
                input: 800,
                cached: 500,
                output: 70
            }
        );
        // input excludes the cached part, like Claude Code's.
        assert_eq!(
            delta.tokens(),
            Tokens {
                input: 300,
                output: 70,
                cache_read: 500,
                cache_write: 0
            }
        );
        // a counter that restarted counts in full
        assert_eq!(ta.since(Some(&tb)), ta);
        assert_eq!(ta.since(None), ta);
    }

    #[test]
    fn codex_model_and_version() {
        let m = codex::turn_context(1, "model-x");
        assert_eq!(codex_model(m.json.as_bytes()).as_deref(), Some("model-x"));
        let s = codex::session_meta(1, "t", "0.167.0", "/w", None);
        assert_eq!(codex_version(s.json.as_bytes()).as_deref(), Some("0.167.0"));
    }

    #[test]
    fn display_text_describes_tools_and_unknowns() {
        let call = claude::assistant_tool(1, "m", "x", Usage::default(), "Bash");
        assert_eq!(
            display_text("claude-code", Kind::Tool, "assistant", call.json.as_bytes()),
            "tool call: Bash"
        );
        let result = claude::tool_result(1, "ok");
        assert_eq!(
            display_text("claude-code", Kind::Tool, "user", result.json.as_bytes()),
            "tool result: ok"
        );
        assert_eq!(
            display_text("codex", Kind::Meta, "event_msg/token_count", b"{}"),
            "(event_msg/token_count)"
        );
        assert_eq!(display_text("someone-else", Kind::User, "x", b"{}"), "(x)");
    }
}
