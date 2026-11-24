use std::sync::Arc;

use nekorisu_archive::{Archive, Error, Kind, LinesQuery, SessionFilter};
use nekorisu_storage::{MemoryProvider, Provider};
use nekorisu_testkit::{ArchiveBuilder, FileSpec, SessionSpec, Usage, claude, codex};

const T0: i64 = 1_794_700_800_000; // 2026-11-15T00:00:00Z

fn claude_lines(n: usize, start: i64) -> Vec<nekorisu_testkit::Line> {
    let mut lines = Vec::new();
    for i in 0..n {
        let ts = start + i as i64 * 10_000;
        lines.push(claude::user(ts, &format!("question {i}")));
        lines.push(claude::assistant_text(
            ts + 1000,
            &format!("m{i}"),
            "model-a",
            Usage::default(),
            &format!("answer {i}"),
        ));
    }
    lines
}

fn archive_with(builder: ArchiveBuilder) -> (Archive, Arc<MemoryProvider>) {
    let memory = Arc::new(builder.into_memory());
    (Archive::new(memory.clone()), memory)
}

fn fixture() -> ArchiveBuilder {
    let mut b = ArchiveBuilder::new();
    b.device("dev-a", "mac-mini", T0 - 1000).device("dev-b", "laptop", T0);

    // A Claude Code session on dev-a, uploaded twice: the second upload adds a segment.
    let first = claude_lines(3, T0);
    let more = claude_lines(2, T0 + 100_000);
    let mut spec = SessionSpec::new(
        "claude-code",
        "s1",
        "dev-a",
        T0 + 200_000,
        vec![FileSpec::transcript("projects/-work-demo/s1.jsonl", first.clone())],
    );
    spec.title = Some("Refactor the parser".into());
    spec.agent_version = "2.1.322".into();
    b.session(spec);
    let mut second = SessionSpec::new(
        "claude-code",
        "s1",
        "dev-a",
        T0 + 300_000,
        vec![FileSpec {
            rel_path: "projects/-work-demo/s1.jsonl".into(),
            role: "transcript",
            segments: vec![first, more],
        }],
    );
    second.seq = 2;
    second.parent_manifest = Some("dev-a-1".into());
    second.title = Some("Refactor the parser".into());
    b.session(second);

    // dev-b continued the same session from manifest dev-a-1: a fork.
    let mut fork = SessionSpec::new(
        "claude-code",
        "s1",
        "dev-b",
        T0 + 400_000,
        vec![FileSpec::transcript(
            "projects/-work-demo/s1.jsonl",
            claude_lines(4, T0),
        )],
    );
    fork.parent_manifest = Some("dev-a-1".into());
    fork.agent_version = "2.1.322".into();
    fork.title = Some("Refactor the parser".into());
    b.session(fork);

    // A Codex thread and its sub-agent thread.
    let mut parent = SessionSpec::new(
        "codex",
        "t1",
        "dev-b",
        T0 + 500_000,
        vec![FileSpec::transcript(
            "sessions/2026/11/15/rollout-t1.jsonl",
            vec![
                codex::session_meta(T0, "t1", "0.167.0", "/work/other", None),
                codex::user(T0 + 1000, "hello codex"),
                codex::agent_message(T0 + 2000, "hello"),
            ],
        )],
    );
    parent.cwd = "/work/other".into();
    b.session(parent);
    let mut child = SessionSpec::new(
        "codex",
        "t2",
        "dev-b",
        T0 + 510_000,
        vec![FileSpec::transcript(
            "sessions/2026/11/15/rollout-t2.jsonl",
            vec![codex::session_meta(
                T0 + 3000,
                "t2",
                "0.167.0",
                "/work/other",
                Some("t1"),
            )],
        )],
    );
    child.seq = 2;
    child.parent_session = Some("codex:t1".into());
    b.session(child);

    b.digest("dg1", "# notes", &["claude-code:s1"], T0 + 600_000);
    b
}

#[tokio::test]
async fn lists_sessions_across_agents_and_devices() {
    let (archive, _) = archive_with(fixture());
    let (total, items) = archive.sessions(&SessionFilter::default()).await.unwrap();
    // The Codex child is hidden by default.
    assert_eq!(total, 2);
    assert_eq!(items.len(), 2);

    let s1 = items.iter().find(|s| s.session_ref == "claude-code:s1").unwrap();
    assert_eq!(s1.title.as_deref(), Some("Refactor the parser"));
    assert_eq!(s1.agent_version.as_deref(), Some("2.1.322"));
    assert_eq!(s1.versions, 3);
    assert_eq!(s1.heads, 2, "dev-a-2 and dev-b-1 both descend from dev-a-1");
    let names: Vec<_> = s1.devices.iter().map(|d| d.name.clone().unwrap()).collect();
    assert_eq!(names, ["mac-mini", "laptop"]);
    assert_eq!(s1.uploaded_ms, T0 + 400_000);

    let t1 = items.iter().find(|s| s.session_ref == "codex:t1").unwrap();
    assert_eq!(t1.children, 1);
    assert_eq!(t1.cwd.as_deref(), Some("/work/other"));

    let filter = SessionFilter {
        include_children: true,
        ..SessionFilter::default()
    };
    assert_eq!(archive.sessions(&filter).await.unwrap().0, 3);

    let by_device = SessionFilter {
        device: Some("dev-a".into()),
        ..SessionFilter::default()
    };
    assert_eq!(archive.sessions(&by_device).await.unwrap().0, 1);
    let by_agent = SessionFilter {
        agent: Some("codex".into()),
        ..SessionFilter::default()
    };
    assert_eq!(archive.sessions(&by_agent).await.unwrap().0, 1);
    let search = SessionFilter {
        q: Some("PARSER".into()),
        ..SessionFilter::default()
    };
    let (n, hits) = archive.sessions(&search).await.unwrap();
    assert_eq!((n, hits[0].id.as_str()), (1, "s1"));
}

#[tokio::test]
async fn paginates_newest_first() {
    let (archive, _) = archive_with(fixture());
    let page = SessionFilter {
        include_children: true,
        limit: 1,
        offset: 1,
        ..SessionFilter::default()
    };
    let (total, items) = archive.sessions(&page).await.unwrap();
    assert_eq!(total, 3);
    assert_eq!(items.len(), 1);
}

#[tokio::test]
async fn session_detail_shows_versions_files_children_and_digests() {
    let (archive, _) = archive_with(fixture());
    let detail = archive.session("claude-code", "s1", None).await.unwrap();
    assert_eq!(detail.selected_version, "dev-b-1", "the latest head");
    assert_eq!(detail.versions.len(), 3);
    assert_eq!(detail.versions[0].id, "dev-b-1");
    assert!(detail.versions.iter().filter(|v| v.is_head).count() == 2);
    assert!(!detail.versions.iter().find(|v| v.id == "dev-a-1").unwrap().is_head);
    assert_eq!(detail.files.len(), 1);
    assert_eq!(detail.files[0].role, "transcript");
    assert_eq!(detail.files[0].lines, Some(8));
    assert_eq!(detail.digests.len(), 1);

    let older = archive.session("claude-code", "s1", Some("dev-a-2")).await.unwrap();
    assert_eq!(older.selected_version, "dev-a-2");
    assert_eq!(older.files[0].segments, 2);
    assert_eq!(older.files[0].lines, Some(10));

    let parent = archive.session("codex", "t1", None).await.unwrap();
    assert_eq!(parent.children.len(), 1);
    assert_eq!(parent.children[0].parent.as_deref(), Some("codex:t1"));

    assert!(matches!(
        archive.session("codex", "nope", None).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        archive.session("claude-code", "s1", Some("zzz")).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn reads_lines_by_number_across_segments() {
    let (archive, _) = archive_with(fixture());
    // dev-a-2 has two segments of 6 and 4 lines.
    let q = LinesQuery {
        from: Some(5),
        to: Some(8),
        ..LinesQuery::default()
    };
    let page = archive.lines("claude-code", "s1", Some("dev-a-2"), &q).await.unwrap();
    assert_eq!(page.total_lines, 10);
    assert_eq!(page.matched, 4);
    assert!(!page.truncated);
    let lines: Vec<u64> = page.items.iter().map(|i| i.line).collect();
    assert_eq!(lines, [5, 6, 7, 8]);
    // Line 7 is the first line of the second segment: "question 0" of the added turns.
    let text = String::from_utf8(page.items[2].raw.to_vec()).unwrap();
    assert!(text.contains("question 0"), "{text}");
    assert_eq!(page.items[2].kind, Kind::User);
    assert_eq!(page.items[0].raw_type, "user");
    assert_eq!(page.items[0].ts_ms, Some(T0 + 20_000));
}

#[tokio::test]
async fn filters_by_kind_with_offset_and_limit() {
    let (archive, _) = archive_with(fixture());
    let q = LinesQuery {
        kinds: vec![Kind::User],
        limit: 2,
        offset: 1,
        ..LinesQuery::default()
    };
    let page = archive.lines("claude-code", "s1", Some("dev-b-1"), &q).await.unwrap();
    assert_eq!(page.total_lines, 8);
    assert_eq!(page.matched, 4);
    assert!(page.truncated, "4 matches, offset 1 + limit 2 shows 3");
    let texts: Vec<String> = page
        .items
        .iter()
        .map(|i| String::from_utf8(i.raw.to_vec()).unwrap())
        .collect();
    assert!(texts[0].contains("question 1") && texts[1].contains("question 2"));
    assert!(page.items.iter().all(|i| i.kind == Kind::User));
}

#[tokio::test]
async fn neighbouring_lines_are_fetched_with_one_request() {
    let mut b = ArchiveBuilder::new();
    b.device("d", "dev", T0);
    b.session(SessionSpec::new(
        "claude-code",
        "big",
        "d",
        T0,
        vec![FileSpec::transcript("projects/p/big.jsonl", claude_lines(200, T0))],
    ));
    let (archive, memory) = archive_with(b);
    let page = archive
        .lines(
            "claude-code",
            "big",
            None,
            &LinesQuery {
                limit: 400,
                ..LinesQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 400);
    let (reads, _) = memory.read_stats();
    // The manifest, the device file, the line map, and ONE merged read of the segment.
    let before = reads;
    let again = archive
        .lines(
            "claude-code",
            "big",
            None,
            &LinesQuery {
                from: Some(1),
                to: Some(50),
                ..LinesQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(again.items.len(), 50);
    let (after, _) = memory.read_stats();
    assert_eq!(after - before, 1, "cached manifest and line map; one ranged read");
    assert!(before <= 5, "reads so far: {before}");
}

#[tokio::test]
async fn a_missing_or_garbled_line_map_is_reported_not_guessed() {
    let build = || {
        let mut b = ArchiveBuilder::new();
        b.session(SessionSpec::new(
            "codex",
            "gap",
            "d",
            T0,
            vec![FileSpec::transcript("a.jsonl", vec![codex::user(T0, "hi")])],
        ));
        b.into_memory()
    };
    let map_path = |p: &MemoryProvider| {
        let dir = "nekorisu/v1/sessions/codex/gap/linemaps";
        futures_lite_block(p.list(dir)).unwrap()[0].path.clone()
    };

    let provider = build();
    let path = map_path(&provider);
    assert!(provider.remove(&path));
    let archive = Archive::new(Arc::new(provider));
    let err = archive
        .lines("codex", "gap", None, &LinesQuery::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::LineMapMissing(_)), "{err}");

    let provider = build();
    let path = map_path(&provider);
    provider.put(&path, b"not json".to_vec());
    let archive = Archive::new(Arc::new(provider));
    let err = archive
        .lines("codex", "gap", None, &LinesQuery::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Format { .. }), "{err}");
}

fn futures_lite_block<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

#[tokio::test]
async fn devices_activity_and_digests() {
    let (archive, _) = archive_with(fixture());
    let devices = archive.devices().await.unwrap();
    assert_eq!(devices.len(), 2);
    let a = devices.iter().find(|d| d.id == "dev-a").unwrap();
    assert_eq!(a.name.as_deref(), Some("mac-mini"));
    assert_eq!(a.sessions, 1);
    assert_eq!(a.last_upload_ms, Some(T0 + 300_000));
    let b = devices.iter().find(|d| d.id == "dev-b").unwrap();
    assert_eq!(b.sessions, 3);

    let (records, skipped) = archive.activity(100).await.unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(records.len(), 5);
    assert!(records.windows(2).all(|w| w[0].at_ms >= w[1].at_ms), "newest first");
    assert_eq!(records[0].op, "archive");
    assert_eq!(archive.activity(2).await.unwrap().0.len(), 2);

    assert_eq!(archive.digest_text("dg1").await.unwrap(), "# notes");
    assert!(archive.digest_text("../x").await.is_err());
}

#[tokio::test]
async fn an_empty_or_unreachable_layout_is_not_a_crash() {
    let archive = Archive::new(Arc::new(MemoryProvider::new()));
    let index = archive.index().await.unwrap();
    assert!(!index.layout_found);
    assert_eq!(index.session_count(), 0);
    assert!(archive.sessions(&SessionFilter::default()).await.unwrap().1.is_empty());
}

#[tokio::test]
async fn broken_files_are_skipped_and_counted() {
    let mut b = fixture();
    b.put("sessions/codex/t1/manifests/broken-9.json", b"{{{".to_vec());
    let (archive, _) = archive_with(b);
    let index = archive.index().await.unwrap();
    assert_eq!(index.skipped, 1);
    assert_eq!(index.session_count(), 3);
}
