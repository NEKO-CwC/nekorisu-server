use std::sync::Arc;

use futures::TryStreamExt;
use nekorisu_archive::Archive;
use nekorisu_export::{ExportRequest, Format, Scope, export_stream, select_fields};
use nekorisu_storage::Provider;
use nekorisu_testkit::{ArchiveBuilder, FileSpec, SessionSpec, Usage, claude, codex};
use serde_json::Value;

const T: i64 = 1_794_700_800_000;

fn u(i: u64, o: u64) -> Usage {
    Usage {
        input: i,
        output: o,
        cache_read: 0,
        cache_write: 0,
    }
}

fn fixture() -> Archive {
    let mut b = ArchiveBuilder::new();
    b.device("d1", "mac-mini", T);

    // Claude Code: two turns, a sub-agent working in the first.
    let main = vec![
        claude::user(T, "first question"),
        claude::assistant_text(T + 1000, "m1", "model-a", u(10, 20), "first answer"),
        claude::user(T + 100_000, "second question"),
        claude::assistant_text(T + 101_000, "m2", "model-a", u(30, 40), "second answer"),
    ];
    let sub = vec![claude::assistant_text(T + 500, "s1", "model-b", u(5, 5), "sub result")];
    let mut c1 = SessionSpec::new(
        "claude-code",
        "c1",
        "d1",
        T + 200_000,
        vec![
            FileSpec::transcript("projects/p/c1.jsonl", main),
            FileSpec {
                rel_path: "projects/p/c1/subagents/agent-a.jsonl".into(),
                role: "subagent",
                segments: vec![sub],
            },
        ],
    );
    c1.cwd = "/work/demo".into();
    b.session(c1);

    // Codex with a child thread.
    let t1 = vec![
        codex::session_meta(T + 300_000, "t1", "0.167.0", "/work/x", None),
        codex::turn_context(T + 300_100, "model-x"),
        codex::user(T + 301_000, "codex question"),
        codex::token_count(T + 302_000, 1000, 200, 100, 0),
        codex::agent_message(T + 303_000, "codex answer"),
    ];
    let mut s = SessionSpec::new(
        "codex",
        "t1",
        "d1",
        T + 400_000,
        vec![FileSpec::transcript("sessions/t1.jsonl", t1)],
    );
    s.cwd = "/work/x".into();
    b.session(s);
    let child = vec![codex::token_count(T + 302_500, 400, 0, 40, 0)];
    let mut s = SessionSpec::new(
        "codex",
        "t2",
        "d1",
        T + 400_001,
        vec![FileSpec::transcript("sessions/t2.jsonl", child)],
    );
    s.seq = 2;
    s.parent_session = Some("codex:t1".into());
    b.session(s);
    Archive::new(Arc::new(b.into_memory()))
}

async fn run(archive: &Arc<Archive>, scope: Scope, fields: &[&str], format: Format, max_rows: Option<usize>) -> String {
    let fields = select_fields(&fields.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    let stream = export_stream(
        archive.clone(),
        ExportRequest {
            scope,
            fields,
            format,
            max_rows,
        },
    );
    let chunks: Vec<_> = stream.try_collect().await.unwrap();
    chunks.iter().map(|c| String::from_utf8(c.to_vec()).unwrap()).collect()
}

fn parse_rows(text: &str) -> Vec<Value> {
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[tokio::test]
async fn exports_every_turn_of_every_session() {
    let archive = Arc::new(fixture());
    let text = run(&archive, Scope::default(), &[], Format::Jsonl, None).await;
    let rows = parse_rows(&text);
    // two Claude turns and one Codex turn; the Codex child is not a row of its own
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["session"], "claude-code:c1");
    assert_eq!(rows[0]["user"], "first question");
    assert_eq!(rows[0]["device"], "mac-mini");
    assert_eq!(rows[0]["cwd"], "/work/demo");
    assert_eq!(rows[0]["subagents"], 1);
    // 10+5 input, 20+5 output: the sub-agent counts in
    assert_eq!(rows[0]["tokens"]["input"], 15);
    assert_eq!(rows[0]["tokens"]["output"], 25);
    assert_eq!(rows[0]["requests"], 2);
    assert_eq!(rows[0]["model"], serde_json::json!(["model-a", "model-b"]));
    assert_eq!(rows[1]["prev_reply"], "first answer");
    assert_eq!(rows[1]["gap_since_reply"], 99_000);
    assert_eq!(rows[1]["parse"], "ok");

    assert_eq!(rows[2]["session"], "codex:t1");
    assert_eq!(rows[2]["agent_version"], serde_json::json!(["0.167.0"]));
    // 1000-200 + 400 = 1200 input; 100+40 = 140 output
    assert_eq!(rows[2]["tokens"]["input"], 1200);
    assert_eq!(rows[2]["tokens"]["output"], 140);
    assert_eq!(rows[2]["tokens"]["cache_read"], 200);
    assert_eq!(rows[2]["subagents"], 1);
}

#[tokio::test]
async fn filters_and_field_choice() {
    let archive = Arc::new(fixture());
    let scope = Scope {
        agent: Some("claude-code".into()),
        since_ms: Some(T + 50_000),
        ..Scope::default()
    };
    let text = run(&archive, scope, &["user", "turn"], Format::Jsonl, None).await;
    let rows = parse_rows(&text);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0], serde_json::json!({ "turn": 2, "user": "second question" }));

    let scope = Scope {
        sessions: vec!["codex:t1".into()],
        ..Scope::default()
    };
    assert_eq!(
        parse_rows(&run(&archive, scope, &["session"], Format::Jsonl, None).await).len(),
        1
    );

    let limited = run(&archive, Scope::default(), &["turn"], Format::Jsonl, Some(2)).await;
    assert_eq!(limited.lines().count(), 2);

    let scope = Scope {
        until_ms: Some(T + 50_000),
        ..Scope::default()
    };
    // Only the first Claude turn started before that time.
    assert_eq!(
        run(&archive, scope, &["turn"], Format::Jsonl, None)
            .await
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn writes_csv_with_a_header() {
    let archive = Arc::new(fixture());
    let text = run(
        &archive,
        Scope::default(),
        &["session", "turn", "tokens"],
        Format::Csv,
        None,
    )
    .await;
    let mut lines = text.split("\r\n");
    assert_eq!(
        lines.next().unwrap(),
        "session,turn,tokens_input,tokens_output,tokens_cache_read,tokens_cache_write"
    );
    assert_eq!(lines.next().unwrap(), "claude-code:c1,1,15,25,0,0");
}

#[tokio::test]
async fn an_unreadable_session_becomes_an_error_row() {
    let mut b = ArchiveBuilder::new();
    b.session(SessionSpec::new(
        "codex",
        "bad",
        "d",
        T,
        vec![FileSpec::transcript("a.jsonl", vec![codex::user(T, "hi")])],
    ));
    let provider = b.into_memory();
    // Break the line map.
    let maps = provider.list("nekorisu/v1/sessions/codex/bad/linemaps").await.unwrap();
    provider.put(&maps[0].path, b"garbage".to_vec());
    let archive = Arc::new(Archive::new(Arc::new(provider)));
    let text = run(&archive, Scope::default(), &["session", "parse"], Format::Jsonl, None).await;
    let rows = parse_rows(&text);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["parse"], "error");

    // Without `parse` in the fields there is no way to show the error in the data, so the stream fails.
    let fields = select_fields(&["session".to_string()]).unwrap();
    let stream = export_stream(
        archive,
        ExportRequest {
            scope: Scope::default(),
            fields,
            format: Format::Jsonl,
            max_rows: None,
        },
    );
    let result: Result<Vec<_>, _> = stream.try_collect().await;
    assert!(result.is_err());
}
