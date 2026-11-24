//! Export and analysis: turns every session into one
//! row per user message and writes them as JSONL or CSV.
//!
//! Read-only, and nothing is kept: rows are computed while they are written,
//! so no copy of any session text is made. Depends on the archive layer's reading
//! interface only.

pub mod adapter;
pub mod fields;
pub mod turns;

use std::sync::Arc;

use bytes::Bytes;
use futures::Stream;
use nekorisu_archive::{Archive, Kind, LinesQuery, Selector, SessionFilter, SessionSummary};

pub use adapter::{AgentKind, Tokens, agent_kind, display_text};
pub use fields::{FIELDS, FieldInfo, select as select_fields};
pub use turns::{Side, TurnRecord, build_turns};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(transparent)]
    Archive(#[from] nekorisu_archive::Error),
    #[error("bad request: {0}")]
    BadRequest(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jsonl,
    Csv,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "jsonl" => Some(Format::Jsonl),
            "csv" => Some(Format::Csv),
            _ => None,
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Format::Jsonl => "application/x-ndjson; charset=utf-8",
            Format::Csv => "text/csv; charset=utf-8",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Jsonl => "jsonl",
            Format::Csv => "csv",
        }
    }
}

/// Which sessions and which turns.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    pub agent: Option<String>,
    /// A device ID.
    pub device: Option<String>,
    /// `<agent>:<id>`; empty = all.
    pub sessions: Vec<String>,
    /// Keep turns whose user message is at or after / before these times.
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct ExportRequest {
    pub scope: Scope,
    pub fields: Vec<&'static str>,
    pub format: Format,
    pub max_rows: Option<usize>,
}

const PAGE: usize = 1000;

fn main_selector(agent: AgentKind) -> Selector {
    match agent {
        AgentKind::ClaudeCode => Arc::new(|kind, raw| kind == Kind::User || raw == "assistant"),
        AgentKind::Codex => Arc::new(|kind, raw| {
            matches!(kind, Kind::User | Kind::Assistant | Kind::Reasoning)
                || matches!(raw, "event_msg/token_count" | "turn_context" | "session_meta")
        }),
        AgentKind::Other => Arc::new(|_, _| false),
    }
}

fn side_selector(agent: AgentKind) -> Selector {
    match agent {
        AgentKind::ClaudeCode => Arc::new(|_, raw| raw == "assistant"),
        _ => Arc::new(|_, raw| matches!(raw, "event_msg/token_count" | "turn_context")),
    }
}

/// Every selected line of one file, a page at a time.
async fn read_selected(
    archive: &Archive,
    agent: &str,
    id: &str,
    version: &str,
    file: &str,
    select: &Selector,
) -> Result<Vec<nekorisu_archive::LineRecord>, ExportError> {
    let mut out = Vec::new();
    loop {
        let query = LinesQuery {
            file: Some(file.to_string()),
            select: Some(select.clone()),
            limit: PAGE,
            offset: out.len(),
            ..LinesQuery::default()
        };
        let page = archive.lines(agent, id, Some(version), &query).await?;
        let got = page.items.len();
        out.extend(page.items);
        if !page.truncated || got == 0 {
            return Ok(out);
        }
    }
}

/// The turns of one session (its newest head), and the device and directory to
/// write beside them.
pub async fn session_turns(archive: &Archive, summary: &SessionSummary) -> Result<Vec<TurnRecord>, ExportError> {
    let agent = agent_kind(&summary.agent);
    let detail = archive.session(&summary.agent, &summary.id, None).await?;
    let version = detail.selected_version.clone();
    let Some(main_file) = detail.files.iter().find(|f| f.role == "transcript") else {
        return Ok(Vec::new());
    };
    let main = read_selected(
        archive,
        &summary.agent,
        &summary.id,
        &version,
        &main_file.path,
        &main_selector(agent),
    )
    .await?;

    let mut side = Vec::new();
    match agent {
        AgentKind::ClaudeCode => {
            for file in detail
                .files
                .iter()
                .filter(|f| f.role == "subagent" && f.path.ends_with(".jsonl"))
            {
                let lines = read_selected(
                    archive,
                    &summary.agent,
                    &summary.id,
                    &version,
                    &file.path,
                    &side_selector(agent),
                )
                .await?;
                side.push(Side {
                    id: file.path.clone(),
                    lines,
                });
            }
        }
        AgentKind::Codex => {
            for child in &detail.children {
                let child_detail = archive.session(&child.agent, &child.id, None).await?;
                let Some(file) = child_detail.files.iter().find(|f| f.role == "transcript") else {
                    continue;
                };
                let lines = read_selected(
                    archive,
                    &child.agent,
                    &child.id,
                    &child_detail.selected_version,
                    &file.path,
                    &side_selector(agent),
                )
                .await?;
                side.push(Side {
                    id: child.session_ref.clone(),
                    lines,
                });
            }
        }
        AgentKind::Other => {}
    }
    Ok(build_turns(&summary.agent, &main, &side))
}

/// Writes the export as a stream of byte chunks, one session at a time.
pub fn export_stream(
    archive: Arc<Archive>,
    request: ExportRequest,
) -> impl Stream<Item = Result<Bytes, ExportError>> + Send + 'static {
    async_stream::try_stream! {
        let filter = SessionFilter {
            agent: request.scope.agent.clone(),
            device: request.scope.device.clone(),
            q: None,
            include_children: false,
            limit: usize::MAX,
            offset: 0,
        };
        let (_, mut sessions) = archive.sessions(&filter).await?;
        if !request.scope.sessions.is_empty() {
            sessions.retain(|s| request.scope.sessions.contains(&s.session_ref));
        }
        // Oldest first, so the rows read like a history.
        sessions.sort_by_key(|s| (s.first_ts_ms.unwrap_or(s.uploaded_ms), s.session_ref.clone()));

        if request.format == Format::Csv {
            yield Bytes::from(fields::csv_header(&request.fields));
        }
        let with_parse = request.fields.contains(&"parse");
        let mut written = 0usize;
        'sessions: for summary in sessions {
            let device = summary.devices.last().map(|d| d.name.clone().unwrap_or_else(|| d.id.clone()));
            let turns = match session_turns(&archive, &summary).await {
                Ok(turns) => turns,
                Err(error) if with_parse => {
                    tracing::warn!(%error, "a session could not be exported");
                    let empty = TurnRecord::default();
                    let row = fields::Row {
                        session: &summary.session_ref,
                        agent: &summary.agent,
                        device: device.as_deref(),
                        cwd: summary.cwd.as_deref(),
                        turn: &empty,
                        error: Some("unreadable"),
                    };
                    yield Bytes::from(render(&row, &request));
                    continue;
                }
                Err(error) => Err(error)?,
            };
            for turn in &turns {
                if request.scope.since_ms.is_some_and(|s| turn.at_ms.is_none_or(|t| t < s))
                    || request.scope.until_ms.is_some_and(|u| turn.at_ms.is_none_or(|t| t >= u))
                {
                    continue;
                }
                let row = fields::Row {
                    session: &summary.session_ref,
                    agent: &summary.agent,
                    device: device.as_deref(),
                    cwd: summary.cwd.as_deref(),
                    turn,
                    error: None,
                };
                yield Bytes::from(render(&row, &request));
                written += 1;
                if request.max_rows.is_some_and(|m| written >= m) {
                    break 'sessions;
                }
            }
        }
    }
}

fn render(row: &fields::Row<'_>, request: &ExportRequest) -> String {
    match request.format {
        Format::Jsonl => fields::jsonl(row, &request.fields),
        Format::Csv => fields::csv_row(row, &request.fields),
    }
}
