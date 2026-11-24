//! Turning a session's records into turns, one per user message: a turn runs from one
//! user message to the next.
//!
//! Tokens, requests, models and thinking of the sub-agents that worked during a
//! turn are counted into that turn. Records are
//! placed in a turn by their timestamp.

use std::collections::{BTreeSet, HashMap, HashSet};

use nekorisu_archive::{Kind, LineRecord};

use crate::adapter::{
    AgentKind, CodexTotals, Tokens, agent_kind, claude_request, claude_version, codex_model, codex_token_total,
    codex_version, message_text,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnRecord {
    /// From 1.
    pub index: u64,
    pub at_ms: Option<i64>,
    pub user: String,
    pub prev_reply: Option<String>,
    pub gap_since_reply_ms: Option<i64>,
    pub gap_since_user_ms: Option<i64>,
    pub models: Vec<String>,
    pub tokens: Tokens,
    pub requests: u64,
    pub subagents: u64,
    pub reasoning_chars: u64,
    pub versions: Vec<String>,
    /// Something this turn needed could not be read.
    pub partial: bool,
}

/// The records of one sub-agent: a Claude Code sub-agent file, or a Codex
/// child thread. `id` tells them apart.
pub struct Side {
    pub id: String,
    pub lines: Vec<LineRecord>,
}

struct Acc {
    rec: TurnRecord,
    models: BTreeSet<String>,
    versions: BTreeSet<String>,
    /// Claude Code: one entry per model request, keyed by (source, message id);
    /// the numbers of records that share an id are not added but maxed.
    requests: HashMap<(String, String), Tokens>,
    side: HashSet<String>,
}

impl Acc {
    fn finish(mut self) -> TurnRecord {
        for tokens in self.requests.values() {
            self.rec.tokens.add(tokens);
        }
        self.rec.requests += self.requests.len() as u64;
        self.rec.subagents = self.side.len() as u64;
        self.rec.models = self.models.into_iter().collect();
        self.rec.versions = self.versions.into_iter().collect();
        self.rec
    }
}

fn diff(later: Option<i64>, earlier: Option<i64>) -> Option<i64> {
    Some(later? - earlier?)
}

fn start_turn(
    index: u64,
    line: &LineRecord,
    text: String,
    last_reply: &Option<(String, Option<i64>)>,
    last_user_ts: Option<i64>,
) -> Acc {
    Acc {
        rec: TurnRecord {
            index,
            at_ms: line.ts_ms,
            user: text,
            prev_reply: last_reply.as_ref().map(|r| r.0.clone()),
            gap_since_reply_ms: diff(line.ts_ms, last_reply.as_ref().and_then(|r| r.1)),
            gap_since_user_ms: diff(line.ts_ms, last_user_ts),
            partial: line.ts_ms.is_none(),
            ..TurnRecord::default()
        },
        models: BTreeSet::new(),
        versions: BTreeSet::new(),
        requests: HashMap::new(),
        side: HashSet::new(),
    }
}

/// The turn a record at `ts` belongs to: the last one that started at or before it.
fn turn_at(accs: &[Acc], ts: i64) -> Option<usize> {
    accs.iter().rposition(|a| a.rec.at_ms.is_some_and(|t| t <= ts))
}

pub fn build_turns(agent: &str, main: &[LineRecord], side: &[Side]) -> Vec<TurnRecord> {
    match agent_kind(agent) {
        AgentKind::ClaudeCode => claude(main, side),
        AgentKind::Codex => codex(main, side),
        AgentKind::Other => Vec::new(),
    }
}

fn finish(accs: Vec<Acc>, unplaced: bool) -> Vec<TurnRecord> {
    accs.into_iter()
        .map(|a| {
            let mut rec = a.finish();
            rec.partial |= unplaced;
            rec
        })
        .collect()
}

fn claude(main: &[LineRecord], side: &[Side]) -> Vec<TurnRecord> {
    let mut accs: Vec<Acc> = Vec::new();
    let mut last_reply: Option<(String, Option<i64>)> = None;
    let mut last_user_ts: Option<i64> = None;
    for line in main {
        if line.kind == Kind::User {
            let Some(text) = message_text(AgentKind::ClaudeCode, Kind::User, &line.raw) else {
                continue;
            };
            let mut acc = start_turn(accs.len() as u64 + 1, line, text, &last_reply, last_user_ts);
            if let Some(v) = claude_version(&line.raw) {
                acc.versions.insert(v);
            }
            if line.ts_ms.is_some() {
                last_user_ts = line.ts_ms;
            }
            accs.push(acc);
            continue;
        }
        if line.raw_type != "assistant" {
            continue;
        }
        if let Some(acc) = accs.last_mut() {
            add_claude_request(acc, "main", line);
        }
        if line.kind == Kind::Assistant
            && let Some(text) = message_text(AgentKind::ClaudeCode, Kind::Assistant, &line.raw)
        {
            last_reply = Some((text, line.ts_ms));
        }
    }
    let mut unplaced = false;
    for s in side {
        for line in s.lines.iter().filter(|l| l.raw_type == "assistant") {
            match line.ts_ms.and_then(|ts| turn_at(&accs, ts)) {
                Some(i) => {
                    add_claude_request(&mut accs[i], &s.id, line);
                    accs[i].side.insert(s.id.clone());
                }
                None => unplaced = true,
            }
        }
    }
    finish(accs, unplaced)
}

fn add_claude_request(acc: &mut Acc, source: &str, line: &LineRecord) {
    let Some(request) = claude_request(&line.raw) else {
        acc.rec.partial = true;
        return;
    };
    // A record without an id is its own request.
    let id = request
        .message_id
        .clone()
        .unwrap_or_else(|| format!("line-{}", line.line));
    let entry = acc.requests.entry((source.to_string(), id)).or_default();
    match &request.usage {
        Some(usage) => entry.max_with(usage),
        None => acc.rec.partial = true,
    }
    if let Some(model) = request.model {
        acc.models.insert(model);
    }
    if let Some(version) = request.version {
        acc.versions.insert(version);
    }
    acc.rec.reasoning_chars += request.reasoning_chars;
}

fn codex(main: &[LineRecord], side: &[Side]) -> Vec<TurnRecord> {
    let mut accs: Vec<Acc> = Vec::new();
    let mut last_reply: Option<(String, Option<i64>)> = None;
    let mut last_user_ts: Option<i64> = None;
    let mut totals: Option<CodexTotals> = None;
    let mut model: Option<String> = None;
    let mut version: Option<String> = None;
    for line in main {
        match (line.kind, line.raw_type.as_str()) {
            (_, "session_meta") => version = codex_version(&line.raw).or(version),
            (_, "turn_context") => {
                if let Some(m) = codex_model(&line.raw) {
                    if let Some(acc) = accs.last_mut() {
                        acc.models.insert(m.clone());
                    }
                    model = Some(m);
                }
            }
            (Kind::User, _) => {
                let Some(text) = message_text(AgentKind::Codex, Kind::User, &line.raw) else {
                    continue;
                };
                let mut acc = start_turn(accs.len() as u64 + 1, line, text, &last_reply, last_user_ts);
                acc.models.extend(model.clone());
                acc.versions.extend(version.clone());
                if line.ts_ms.is_some() {
                    last_user_ts = line.ts_ms;
                }
                accs.push(acc);
            }
            (Kind::Assistant, _) => {
                if let Some(text) = message_text(AgentKind::Codex, Kind::Assistant, &line.raw) {
                    last_reply = Some((text, line.ts_ms));
                }
            }
            (Kind::Reasoning, _) => {
                if let (Some(acc), Some(text)) = (
                    accs.last_mut(),
                    message_text(AgentKind::Codex, Kind::Reasoning, &line.raw),
                ) {
                    acc.rec.reasoning_chars += text.chars().count() as u64;
                }
            }
            (_, "event_msg/token_count") => {
                if let Some(now) = codex_token_total(&line.raw) {
                    let delta = now.since(totals.as_ref());
                    totals = Some(now);
                    if let Some(acc) = accs.last_mut()
                        && delta != CodexTotals::default()
                    {
                        acc.rec.tokens.add(&delta.tokens());
                        acc.rec.requests += 1;
                    }
                }
            }
            _ => {}
        }
    }
    let mut unplaced = false;
    for s in side {
        let mut totals: Option<CodexTotals> = None;
        for line in &s.lines {
            let at = line.ts_ms.and_then(|ts| turn_at(&accs, ts));
            match line.raw_type.as_str() {
                "event_msg/token_count" => {
                    let Some(now) = codex_token_total(&line.raw) else {
                        continue;
                    };
                    let delta = now.since(totals.as_ref());
                    totals = Some(now);
                    if delta == CodexTotals::default() {
                        continue;
                    }
                    match at {
                        Some(i) => {
                            accs[i].rec.tokens.add(&delta.tokens());
                            accs[i].rec.requests += 1;
                            accs[i].side.insert(s.id.clone());
                        }
                        None => unplaced = true,
                    }
                }
                "turn_context" => {
                    if let (Some(i), Some(m)) = (at, codex_model(&line.raw)) {
                        accs[i].models.insert(m);
                    }
                }
                _ => {}
            }
        }
    }
    finish(accs, unplaced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use nekorisu_testkit::{Line, Usage, claude, codex};

    const T: i64 = 1_794_700_800_000;

    fn record(n: u64, line: &Line) -> LineRecord {
        LineRecord {
            line: n,
            kind: Kind::parse(line.kind).unwrap(),
            raw_type: line.raw_type.clone(),
            ts_ms: (line.ts_ms > 0).then_some(line.ts_ms),
            raw: Bytes::from(line.json.clone().into_bytes()),
        }
    }

    fn records(lines: &[Line]) -> Vec<LineRecord> {
        lines.iter().enumerate().map(|(i, l)| record(i as u64 + 1, l)).collect()
    }

    fn u(input: u64, output: u64, read: u64, write: u64) -> Usage {
        Usage {
            input,
            output,
            cache_read: read,
            cache_write: write,
        }
    }

    #[test]
    fn claude_turns_link_replies_and_dedupe_requests() {
        let lines = vec![
            claude::user(T, "first"),
            // one request, written as three records that share an id
            claude::assistant_thinking(T + 1000, "m1", "model-a", u(10, 1, 100, 5), "think"),
            claude::assistant_tool(T + 2000, "m1", "model-a", u(10, 7, 100, 5), "Bash"),
            claude::tool_result(T + 3000, "ok"),
            claude::assistant_text(T + 4000, "m1", "model-a", u(10, 30, 100, 5), "reply one"),
            claude::user(T + 60_000, "second"),
            claude::assistant_text(T + 61_000, "m2", "model-b", u(20, 40, 0, 0), "reply two"),
        ];
        let turns = build_turns("claude-code", &records(&lines), &[]);
        assert_eq!(turns.len(), 2);

        let t1 = &turns[0];
        assert_eq!((t1.index, t1.user.as_str()), (1, "first"));
        assert_eq!(t1.prev_reply, None);
        assert_eq!(t1.requests, 1, "three records, one request");
        assert_eq!(
            t1.tokens,
            Tokens {
                input: 10,
                output: 30,
                cache_read: 100,
                cache_write: 5
            }
        );
        assert_eq!(t1.models, ["model-a"]);
        assert_eq!(t1.reasoning_chars, 5);
        assert_eq!(t1.versions, ["2.1.322"]);
        assert!(!t1.partial);

        let t2 = &turns[1];
        assert_eq!(t2.prev_reply.as_deref(), Some("reply one"));
        assert_eq!(t2.gap_since_reply_ms, Some(56_000));
        assert_eq!(t2.gap_since_user_ms, Some(60_000));
        assert_eq!(
            t2.tokens,
            Tokens {
                input: 20,
                output: 40,
                cache_read: 0,
                cache_write: 0
            }
        );
        assert_eq!(t2.models, ["model-b"]);
    }

    #[test]
    fn claude_sub_agents_count_into_the_turn_they_ran_in() {
        let main = vec![
            claude::user(T, "first"),
            claude::assistant_text(T + 1000, "m1", "model-a", u(1, 1, 0, 0), "a"),
            claude::user(T + 100_000, "second"),
            claude::assistant_text(T + 101_000, "m2", "model-a", u(1, 1, 0, 0), "b"),
        ];
        let sub_a = vec![
            claude::sidechain_user(T + 2000, "do a thing"),
            claude::assistant_text(T + 3000, "s1", "model-c", u(100, 10, 0, 0), "done"),
        ];
        let sub_b = vec![claude::assistant_text(
            T + 102_000,
            "s1",
            "model-c",
            u(7, 7, 0, 0),
            "done",
        )];
        let side = vec![
            Side {
                id: "agent-a".into(),
                lines: records(&sub_a),
            },
            // same message id as in agent-a, but another file: a different request
            Side {
                id: "agent-b".into(),
                lines: records(&sub_b),
            },
        ];
        let turns = build_turns("claude-code", &records(&main), &side);
        assert_eq!(turns[0].subagents, 1);
        assert_eq!(turns[0].tokens.input, 1 + 100);
        assert_eq!(turns[0].requests, 2);
        assert!(turns[0].models.contains(&"model-c".to_string()));
        assert_eq!(turns[1].subagents, 1);
        assert_eq!(turns[1].tokens.input, 1 + 7);
        assert!(!turns[0].partial && !turns[1].partial);
    }

    #[test]
    fn claude_records_that_cannot_be_placed_make_turns_partial() {
        let main = vec![claude::user(T, "first")];
        let mut orphan = claude::assistant_text(0, "s1", "m", u(1, 1, 0, 0), "x");
        orphan.ts_ms = 0; // no timestamp
        let side = vec![Side {
            id: "agent-a".into(),
            lines: records(&[orphan]),
        }];
        let turns = build_turns("claude-code", &records(&main), &side);
        assert!(turns[0].partial);
    }

    #[test]
    fn codex_takes_deltas_of_cumulative_totals() {
        let lines = vec![
            codex::session_meta(T, "t1", "0.167.0", "/w", None),
            codex::turn_context(T + 100, "model-x"),
            codex::user(T + 1000, "first"),
            codex::reasoning(T + 2000, "abc"),
            codex::command(T + 3000),
            codex::token_count(T + 4000, 1000, 400, 50, 10),
            codex::agent_message(T + 5000, "reply one"),
            codex::token_count(T + 6000, 1800, 900, 120, 20),
            codex::user(T + 70_000, "second"),
            codex::agent_message(T + 71_000, "reply two"),
            codex::token_count(T + 72_000, 2000, 1000, 200, 20),
            codex::token_count(T + 73_000, 2000, 1000, 200, 20), // nothing new
        ];
        let turns = build_turns("codex", &records(&lines), &[]);
        assert_eq!(turns.len(), 2);
        let t1 = &turns[0];
        assert_eq!(t1.models, ["model-x"]);
        assert_eq!(t1.versions, ["0.167.0"]);
        assert_eq!(t1.requests, 2);
        // totals 1800/900/120 minus nothing: input 1800 - cached 900
        assert_eq!(
            t1.tokens,
            Tokens {
                input: 900,
                output: 120,
                cache_read: 900,
                cache_write: 0
            }
        );
        assert_eq!(t1.reasoning_chars, 3);
        let t2 = &turns[1];
        assert_eq!(t2.prev_reply.as_deref(), Some("reply one"));
        assert_eq!(t2.gap_since_reply_ms, Some(65_000));
        assert_eq!(t2.requests, 1, "the repeated total adds nothing");
        assert_eq!(
            t2.tokens,
            Tokens {
                input: 100,
                output: 80,
                cache_read: 100,
                cache_write: 0
            }
        );
    }

    #[test]
    fn codex_child_threads_count_into_the_turn_they_ran_in() {
        let main = vec![
            codex::user(T + 1000, "first"),
            codex::token_count(T + 2000, 100, 0, 10, 0),
            codex::user(T + 50_000, "second"),
        ];
        let child = vec![
            codex::token_count(T + 3000, 500, 100, 50, 0),
            codex::token_count(T + 4000, 900, 300, 80, 0),
        ];
        let side = vec![Side {
            id: "codex:t2".into(),
            lines: records(&child),
        }];
        let turns = build_turns("codex", &records(&main), &side);
        assert_eq!(turns[0].subagents, 1);
        assert_eq!(turns[0].requests, 1 + 2);
        assert_eq!(
            turns[0].tokens,
            Tokens {
                input: 100 + 400 + 200,
                output: 10 + 50 + 30,
                cache_read: 300,
                cache_write: 0
            }
        );
        assert_eq!(turns[1].subagents, 0);
    }

    #[test]
    fn unknown_agents_give_no_rows() {
        let lines = vec![claude::user(T, "x")];
        assert!(build_turns("somebody", &records(&lines), &[]).is_empty());
    }
}
