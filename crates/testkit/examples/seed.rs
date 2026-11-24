//! Writes an invented archive to a directory, for local development:
//!
//!     cargo run -p nekorisu-testkit --example seed -- dev-data/dav
//!     cargo run -p nekorisu-testkit --example dev_webdav -- dev-data/dav
//!
//! then point NEKORISU_WEBDAV_URL at http://127.0.0.1:4918/. Nothing here is
//! real: the repository is public.

use std::path::PathBuf;

use nekorisu_testkit::{ArchiveBuilder, FileSpec, Line, SessionSpec, Usage, claude, codex};
use serde_json::json;

const T: i64 = 1_794_700_800_000; // 2026-11-15T00:00:00Z
const MIN: i64 = 60_000;

fn u(input: u64, output: u64, read: u64, write: u64) -> Usage {
    Usage {
        input,
        output,
        cache_read: read,
        cache_write: write,
    }
}

fn claude_turns(start: i64, topics: &[&str]) -> Vec<Line> {
    let mut lines = Vec::new();
    for (i, topic) in topics.iter().enumerate() {
        let t = start + i as i64 * 4 * MIN;
        let id = format!("msg_{start}_{i}");
        lines.push(claude::user(
            t,
            &format!("Please look at {topic} and tell me what you find."),
        ));
        lines.push(claude::assistant_thinking(
            t + 2_000,
            &id,
            "model-large",
            u(40, 12, 900, 300),
            &format!("Thinking about {topic}."),
        ));
        lines.push(claude::assistant_tool(
            t + 4_000,
            &id,
            "model-large",
            u(40, 30, 900, 300),
            "Read",
        ));
        lines.push(claude::tool_result(
            t + 5_000,
            &format!("(synthetic output about {topic})"),
        ));
        let id2 = format!("{id}b");
        lines.push(claude::assistant_text(
            t + 9_000,
            &id2,
            "model-large",
            u(55, 120 + 10 * i as u64, 1500, 0),
            &format!("Here is what I found about {topic}: nothing alarming, three small things to tidy."),
        ));
    }
    lines
}

fn main() -> std::io::Result<()> {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "dev-data/dav".into()));
    let mut b = ArchiveBuilder::new();
    b.device("5b0e6c3a-1111-4a6e-9a10-000000000001", "mac-mini", T - 86_400_000)
        .device("9d4f2b7e-2222-4c1d-8f20-000000000002", "thinkpad", T - 3_600_000);
    let mini = "5b0e6c3a-1111-4a6e-9a10-000000000001";
    let pad = "9d4f2b7e-2222-4c1d-8f20-000000000002";

    // A Claude Code session on the mac-mini, uploaded twice, with a sub-agent.
    let first = claude_turns(T, &["the parser", "the config loader"]);
    let mut more = first.clone();
    more.extend(claude_turns(T + 20 * MIN, &["the export code"]));
    let sub = vec![
        claude::sidechain_user(T + 5 * MIN, "Search the repository for TODO comments."),
        claude::assistant_text(
            T + 5 * MIN + 3_000,
            "msg_sub_1",
            "model-small",
            u(20, 40, 0, 0),
            "Found 4 TODO comments.",
        ),
    ];
    let mut s = SessionSpec::new(
        "claude-code",
        "73ffc2c4-0000-4000-8000-00000000a001",
        mini,
        T + 10 * MIN,
        vec![FileSpec::transcript(
            "projects/-work-demo/73ffc2c4.jsonl",
            first.clone(),
        )],
    );
    s.title = Some("Tidy the parser and config loader".into());
    s.agent_version = "2.1.322".into();
    s.cwd = "/work/demo".into();
    b.session(s);
    let mut s = SessionSpec::new(
        "claude-code",
        "73ffc2c4-0000-4000-8000-00000000a001",
        mini,
        T + 40 * MIN,
        vec![
            FileSpec {
                rel_path: "projects/-work-demo/73ffc2c4.jsonl".into(),
                role: "transcript",
                segments: vec![first.clone(), claude_turns(T + 20 * MIN, &["the export code"])],
            },
            FileSpec {
                rel_path: "projects/-work-demo/73ffc2c4/subagents/agent-a1.jsonl".into(),
                role: "subagent",
                segments: vec![sub],
            },
        ],
    );
    s.seq = 2;
    s.parent_manifest = Some(format!("{mini}-1"));
    s.title = Some("Tidy the parser and config loader".into());
    s.agent_version = "2.1.322".into();
    s.cwd = "/work/demo".into();
    b.session(s);

    // The thinkpad continued the same session from the first upload: a fork.
    let mut fork_lines = first.clone();
    fork_lines.extend(claude_turns(T + 30 * MIN, &["the release notes"]));
    let mut s = SessionSpec::new(
        "claude-code",
        "73ffc2c4-0000-4000-8000-00000000a001",
        pad,
        T + 60 * MIN,
        vec![FileSpec::transcript("projects/-work-demo/73ffc2c4.jsonl", fork_lines)],
    );
    s.parent_manifest = Some(format!("{mini}-1"));
    s.title = Some("Tidy the parser and config loader".into());
    s.agent_version = "2.1.322".into();
    s.cwd = "/work/demo".into();
    b.session(s);

    // A second, small Claude Code session on the thinkpad.
    let mut s = SessionSpec::new(
        "claude-code",
        "c0ffee00-0000-4000-8000-00000000a002",
        pad,
        T + 2 * 3_600_000,
        vec![FileSpec::transcript(
            "projects/-work-notes/c0ffee00.jsonl",
            claude_turns(T + 90 * MIN, &["the meeting notes"]),
        )],
    );
    s.title = Some("Summarize the meeting notes".into());
    s.cwd = "/work/notes".into();
    s.agent_version = "2.1.327".into();
    b.session(s);

    // A Codex thread with a child thread, on the mac-mini.
    let t = T + 3 * 3_600_000;
    let parent = vec![
        codex::session_meta(
            t,
            "019a0000-0000-7000-8000-00000000b001",
            "0.167.0",
            "/work/other",
            None,
        ),
        codex::turn_context(t + 500, "model-codex"),
        codex::user(t + 1_000, "Add a retry to the uploader."),
        codex::reasoning(t + 2_000, "Planning the retry."),
        codex::command(t + 3_000),
        codex::token_count(t + 4_000, 2_000, 800, 150, 40),
        codex::agent_message(t + 5_000, "Added a retry with backoff."),
        codex::user(t + 5 * MIN, "Now add a test for it."),
        codex::command(t + 5 * MIN + 2_000),
        codex::token_count(t + 5 * MIN + 3_000, 3_500, 2_000, 260, 60),
        codex::agent_message(t + 5 * MIN + 4_000, "Added a test."),
    ];
    let mut s = SessionSpec::new(
        "codex",
        "019a0000-0000-7000-8000-00000000b001",
        mini,
        t + 10 * MIN,
        vec![FileSpec::transcript(
            "sessions/2026/11/15/rollout-019a0000-b001.jsonl",
            parent,
        )],
    );
    s.cwd = "/work/other".into();
    s.agent_version = "0.167.0".into();
    b.session(s);
    let child = vec![
        codex::session_meta(
            t + 2_500,
            "019a0000-0000-7000-8000-00000000b002",
            "0.167.0",
            "/work/other",
            Some("019a0000-0000-7000-8000-00000000b001"),
        ),
        codex::token_count(t + 3_500, 600, 100, 50, 0),
    ];
    let mut s = SessionSpec::new(
        "codex",
        "019a0000-0000-7000-8000-00000000b002",
        mini,
        t + 10 * MIN + 1,
        vec![FileSpec::transcript(
            "sessions/2026/11/15/rollout-019a0000-b002.jsonl",
            child,
        )],
    );
    s.seq = 2;
    s.parent_session = Some("codex:019a0000-0000-7000-8000-00000000b001".into());
    s.cwd = "/work/other".into();
    s.agent_version = "0.167.0".into();
    b.session(s);

    b.digest(
        "dg-parser",
        "# Tidy the parser\n\nA synthetic digest: three small things were tidied.\n",
        &["claude-code:73ffc2c4-0000-4000-8000-00000000a001"],
        T + 45 * MIN,
    );
    b.log_record(
        mini,
        json!({ "seq": 99, "device": mini, "at_ms": T + 50 * MIN, "op": "note", "sessions": ["claude-code:73ffc2c4-0000-4000-8000-00000000a001"], "tags": ["parser"], "note": "synthetic note" }),
    );

    std::fs::create_dir_all(&out)?;
    b.write_to_dir(&out)?;
    println!("wrote a synthetic archive under {}", out.display());
    Ok(())
}
