//! Reading lines of a session by number or kind, straight from the remote
//! segments: the line maps say where each line is, and only those byte ranges
//! are fetched.

use std::ops::Range;
use std::sync::Arc;

use bytes::Bytes;
use futures::{StreamExt, TryStreamExt, stream};

use crate::index::Archive;
use crate::layout;
use crate::linemap::{Kind, LineEntry, LineMap};
use crate::model::{ManifestFile, Role, SegmentRef};
use crate::{Error, Result};

/// Lines closer together than this are fetched with one request.
const MERGE_GAP: u64 = 64 * 1024;
/// A merged request never grows past this (one longer line is fetched alone).
const MAX_READ: u64 = 8 * 1024 * 1024;
const MAX_LIMIT: usize = 1000;
const CONCURRENCY: usize = 8;

/// Picks lines by kind and by the agent's own record type, e.g. every
/// `assistant` record whatever its kind (token counts hide in those).
pub type Selector = Arc<dyn Fn(Kind, &str) -> bool + Send + Sync>;

#[derive(Clone)]
pub struct LinesQuery {
    /// A path from the manifest; default: the first transcript.
    pub file: Option<String>,
    /// 1-based, inclusive.
    pub from: Option<u64>,
    pub to: Option<u64>,
    /// Empty = every kind (unless `select` is set).
    pub kinds: Vec<Kind>,
    /// Lines `select` accepts match too. With `kinds` empty and no `select`,
    /// every line matches.
    pub select: Option<Selector>,
    pub limit: usize,
    /// Counted over the lines that match.
    pub offset: usize,
}

impl Default for LinesQuery {
    fn default() -> Self {
        Self {
            file: None,
            from: None,
            to: None,
            kinds: Vec::new(),
            select: None,
            limit: 100,
            offset: 0,
        }
    }
}

impl std::fmt::Debug for LinesQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinesQuery")
            .field("file", &self.file)
            .field("from", &self.from)
            .field("to", &self.to)
            .field("kinds", &self.kinds)
            .field("select", &self.select.is_some())
            .field("limit", &self.limit)
            .field("offset", &self.offset)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct LineRecord {
    /// 1-based, over the whole file.
    pub line: u64,
    pub kind: Kind,
    pub raw_type: String,
    pub ts_ms: Option<i64>,
    /// The line as stored, without the line break.
    pub raw: Bytes,
}

#[derive(Clone, Debug)]
pub struct LinesPage {
    pub file: String,
    pub version: String,
    pub total_lines: u64,
    pub matched: u64,
    /// More lines matched than `limit` allowed.
    pub truncated: bool,
    pub items: Vec<LineRecord>,
}

struct Picked {
    line: u64,
    segment: usize,
    entry: LineEntry,
}

impl Archive {
    pub async fn lines(&self, agent: &str, id: &str, version: Option<&str>, query: &LinesQuery) -> Result<LinesPage> {
        let index = self.index().await?;
        let entry = index
            .entry(agent, id)
            .ok_or_else(|| Error::NotFound(format!("session {agent}:{id}")))?;
        let manifest = entry.pick(version)?.clone();
        let file = pick_file(&manifest.files, query.file.as_deref())?.clone();
        let maps = self.linemaps_for(agent, id, &file).await?;

        let limit = query.limit.clamp(1, MAX_LIMIT);
        let from = query.from.unwrap_or(1).max(1);
        let to = query.to.unwrap_or(u64::MAX);
        let mut global = 0u64;
        let mut matched = 0u64;
        let mut picked: Vec<Picked> = Vec::new();
        for (segment, map) in maps.iter().enumerate() {
            for line in &map.lines {
                global += 1;
                if global < from || global > to {
                    continue;
                }
                let filtered = !query.kinds.is_empty() || query.select.is_some();
                if filtered
                    && !(query.kinds.contains(&line.kind)
                        || query.select.as_ref().is_some_and(|f| f(line.kind, map.raw_type(line))))
                {
                    continue;
                }
                matched += 1;
                if matched as usize > query.offset && picked.len() < limit {
                    picked.push(Picked {
                        line: global,
                        segment,
                        entry: *line,
                    });
                }
            }
        }
        let truncated = matched as usize > query.offset + picked.len();
        let raws = self.fetch_lines(agent, id, &file, &picked).await?;
        let items = picked
            .iter()
            .zip(raws)
            .map(|(p, raw)| LineRecord {
                line: p.line,
                kind: p.entry.kind,
                raw_type: maps[p.segment].raw_type(&p.entry).to_string(),
                ts_ms: (p.entry.ts_ms > 0).then_some(p.entry.ts_ms),
                raw,
            })
            .collect();
        Ok(LinesPage {
            file: file.path,
            version: manifest.id.clone(),
            total_lines: global,
            matched,
            truncated,
            items,
        })
    }

    /// The line maps of every segment of `file`, in byte order. Cached for good
    /// by the segment's SHA-256: the same bytes have the same map.
    pub(crate) async fn linemaps_for(&self, agent: &str, id: &str, file: &ManifestFile) -> Result<Vec<Arc<LineMap>>> {
        stream::iter(file.segments.iter().cloned())
            .map(|segment| async move { self.linemap(agent, id, &segment).await })
            .buffered(CONCURRENCY)
            .try_collect()
            .await
    }

    async fn linemap(&self, agent: &str, id: &str, segment: &SegmentRef) -> Result<Arc<LineMap>> {
        if let Some(hit) = self.linemaps.lock().unwrap().get(&segment.sha256) {
            return Ok(hit.clone());
        }
        let path = layout::linemap_path(agent, id, &segment.sha256)?;
        let bytes = match self.provider().read(&path, None).await {
            Ok(b) => b,
            Err(nekorisu_storage::Error::NotFound(_)) => return Err(Error::LineMapMissing(path)),
            Err(e) => return Err(e.into()),
        };
        let map = Arc::new(LineMap::from_json(&bytes)?);
        self.linemaps
            .lock()
            .unwrap()
            .insert(segment.sha256.clone(), map.clone());
        Ok(map)
    }

    /// The bytes of the picked lines, in the order they were picked.
    async fn fetch_lines(&self, agent: &str, id: &str, file: &ManifestFile, picked: &[Picked]) -> Result<Vec<Bytes>> {
        // Group by segment, then merge neighbours into one request each.
        /// (index into `picked`, offset inside the range, length)
        type Placed = (usize, u64, u64);
        struct Job {
            segment: usize,
            range: Range<u64>,
            lines: Vec<Placed>,
        }
        let mut order: Vec<usize> = (0..picked.len()).collect();
        order.sort_by_key(|i| (picked[*i].segment, picked[*i].entry.offset));
        let mut jobs: Vec<Job> = Vec::new();
        for i in order {
            let p = &picked[i];
            let (start, end) = (p.entry.offset, p.entry.offset + p.entry.len);
            match jobs.last_mut() {
                Some(job)
                    if job.segment == p.segment
                        && start <= job.range.end + MERGE_GAP
                        && end.max(job.range.end) - job.range.start <= MAX_READ =>
                {
                    job.range.end = job.range.end.max(end);
                    job.lines.push((i, start - job.range.start, p.entry.len));
                }
                _ => jobs.push(Job {
                    segment: p.segment,
                    range: start..end,
                    lines: vec![(i, 0, p.entry.len)],
                }),
            }
        }
        let results: Vec<(Vec<Placed>, Bytes)> = stream::iter(jobs)
            .map(|job| async move {
                let seg = &file.segments[job.segment];
                let path = layout::segment_path(agent, id, &file.path, seg.start, &seg.sha256)?;
                let bytes = self.provider().read(&path, Some(job.range.clone())).await?;
                if (bytes.len() as u64) < job.range.end - job.range.start {
                    return Err(Error::format(
                        "segment",
                        format!("{path} is shorter than its line map says"),
                    ));
                }
                Ok::<_, Error>((job.lines, bytes))
            })
            .buffered(CONCURRENCY)
            .try_collect()
            .await?;
        let mut out = vec![Bytes::new(); picked.len()];
        for (lines, bytes) in results {
            for (i, offset, len) in lines {
                out[i] = bytes.slice(offset as usize..(offset + len) as usize);
            }
        }
        Ok(out)
    }
}

fn pick_file<'a>(files: &'a [ManifestFile], wanted: Option<&str>) -> Result<&'a ManifestFile> {
    match wanted {
        Some(path) => files
            .iter()
            .find(|f| f.path == path)
            .ok_or_else(|| Error::NotFound(format!("file {path}"))),
        None => files
            .iter()
            .find(|f| Archive::role_is_transcript(f.role))
            .or_else(|| {
                files
                    .iter()
                    .find(|f| f.role != Role::Other && f.path.ends_with(".jsonl"))
            })
            .ok_or_else(|| Error::NotFound("this version has no transcript".to_string())),
    }
}
