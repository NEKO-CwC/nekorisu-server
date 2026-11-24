use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use futures::{StreamExt, stream};
use nekorisu_storage::{self as storage, Entry, Provider};
use serde::Serialize;

use crate::json;
use crate::layout;
use crate::linemap::LineMap;
use crate::logs::{self, LogRecord};
use crate::model::{DigestInfo, ForkFrom, Manifest, Role, SessionMeta};
use crate::{Error, Result};

const CONCURRENCY: usize = 16;
/// How long an index is served before the next question lists the storage again.
const DEFAULT_TTL: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DeviceRef {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub id: String,
    pub name: Option<String>,
    pub first_seen_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceStats {
    pub id: String,
    pub name: Option<String>,
    pub first_seen_ms: Option<i64>,
    pub sessions: u64,
    pub bytes: u64,
    pub last_upload_ms: Option<i64>,
}

/// The shapes below are what the panel API sends (docs/api.md).
#[derive(Clone, Debug, Serialize)]
pub struct SessionSummary {
    #[serde(rename = "ref")]
    pub session_ref: String,
    pub agent: String,
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub agent_version: Option<String>,
    pub devices: Vec<DeviceRef>,
    pub first_ts_ms: Option<i64>,
    pub last_ts_ms: Option<i64>,
    pub uploaded_ms: i64,
    pub bytes: u64,
    pub files: usize,
    pub versions: usize,
    pub heads: usize,
    pub parent: Option<String>,
    pub children: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct VersionInfo {
    pub id: String,
    pub parent: Option<String>,
    pub device: DeviceRef,
    pub created_ms: i64,
    pub rewrite: bool,
    pub bytes: u64,
    pub is_head: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct FileInfo {
    pub path: String,
    pub role: &'static str,
    pub bytes: u64,
    pub lines: Option<u64>,
    pub segments: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ForkInfo {
    pub session: String,
    pub byte: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct DigestRef {
    pub id: String,
    pub updated_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionDetail {
    pub summary: SessionSummary,
    pub selected_version: String,
    pub versions: Vec<VersionInfo>,
    pub files: Vec<FileInfo>,
    pub children: Vec<SessionSummary>,
    pub fork_from: Option<ForkInfo>,
    pub digests: Vec<DigestRef>,
}

#[derive(Clone, Debug)]
pub struct SessionFilter {
    pub agent: Option<String>,
    pub device: Option<String>,
    pub q: Option<String>,
    pub include_children: bool,
    pub limit: usize,
    pub offset: usize,
}

impl Default for SessionFilter {
    fn default() -> Self {
        Self {
            agent: None,
            device: None,
            q: None,
            include_children: false,
            limit: 50,
            offset: 0,
        }
    }
}

type ByRef = HashMap<(String, String), usize>;
type Children = HashMap<String, Vec<usize>>;
type ParsedLog = Arc<(Vec<LogRecord>, u64)>;

pub(crate) struct SessionEntry {
    pub agent: String,
    pub id: String,
    /// Oldest first.
    pub manifests: Vec<Arc<Manifest>>,
}

impl SessionEntry {
    /// Manifests no other manifest names as its parent. More than one is a fork.
    pub fn heads(&self) -> Vec<&Arc<Manifest>> {
        let parents: HashSet<&str> = self.manifests.iter().filter_map(|m| m.parent.as_deref()).collect();
        let heads: Vec<_> = self
            .manifests
            .iter()
            .filter(|m| !parents.contains(m.id.as_str()))
            .collect();
        if heads.is_empty() {
            self.manifests.iter().collect()
        } else {
            heads
        }
    }

    pub fn latest_head(&self) -> Option<&Arc<Manifest>> {
        self.heads().into_iter().max_by_key(|m| m.created_ms)
    }

    pub fn pick(&self, version: Option<&str>) -> Result<&Arc<Manifest>> {
        match version {
            Some(v) => self
                .manifests
                .iter()
                .find(|m| m.id == v)
                .ok_or_else(|| Error::NotFound(format!("version {v}"))),
            None => self
                .latest_head()
                .ok_or_else(|| Error::NotFound(format!("{}:{} has no manifest", self.agent, self.id))),
        }
    }
}

pub struct Index {
    pub refreshed_ms: i64,
    /// Whether `nekorisu/v1` exists in the storage.
    pub layout_found: bool,
    pub layout_entries: Vec<String>,
    pub devices: Vec<DeviceInfo>,
    pub digests: Vec<DigestInfo>,
    /// Files that could not be read or understood during the last refresh.
    pub skipped: u64,
    pub(crate) sessions: Vec<SessionEntry>,
    by_ref: ByRef,
    /// parent `<agent>:<id>` -> indices of its child sessions.
    children: Children,
}

impl Index {
    fn lookups(sessions: Vec<SessionEntry>) -> (Vec<SessionEntry>, ByRef, Children) {
        let mut by_ref = HashMap::new();
        let mut children: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, s) in sessions.iter().enumerate() {
            by_ref.insert((s.agent.clone(), s.id.clone()), i);
            if let Some(parent) = s.latest_head().and_then(|m| m.parent_session.as_deref()) {
                children.entry(normalize_ref(parent, &s.agent)).or_default().push(i);
            }
        }
        (sessions, by_ref, children)
    }

    pub(crate) fn entry(&self, agent: &str, id: &str) -> Option<&SessionEntry> {
        self.by_ref
            .get(&(agent.to_string(), id.to_string()))
            .map(|i| &self.sessions[*i])
    }

    pub fn device_ref(&self, id: &str) -> DeviceRef {
        DeviceRef {
            id: id.to_string(),
            name: self.devices.iter().find(|d| d.id == id).and_then(|d| d.name.clone()),
        }
    }

    /// `(agent, id)` of every session with this id, for a bare id typed without its agent.
    pub fn find_id(&self, id: &str) -> Vec<(String, String)> {
        self.sessions
            .iter()
            .filter(|s| s.id == id)
            .map(|s| (s.agent.clone(), s.id.clone()))
            .collect()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn root_session_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.latest_head().is_none_or(|m| m.parent_session.is_none()))
            .count()
    }

    pub fn total_bytes(&self) -> u64 {
        self.sessions
            .iter()
            .filter_map(|s| s.latest_head())
            .map(|m| m.bytes())
            .sum()
    }

    pub fn sessions_by_agent(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for s in &self.sessions {
            *counts.entry(s.agent.as_str()).or_default() += 1;
        }
        let mut out: Vec<_> = counts.into_iter().map(|(a, n)| (a.to_string(), n)).collect();
        out.sort();
        out
    }

    pub fn last_upload_ms(&self) -> Option<i64> {
        self.sessions
            .iter()
            .flat_map(|s| s.manifests.iter())
            .map(|m| m.created_ms)
            .max()
    }

    pub(crate) fn summary(&self, e: &SessionEntry) -> SessionSummary {
        let heads = e.heads();
        let head = e.latest_head();
        // The newest value of a field that the head may not have.
        let pick = |f: &dyn Fn(&SessionMeta) -> Option<String>| -> Option<String> {
            head.and_then(|m| f(&m.session))
                .or_else(|| e.manifests.iter().rev().find_map(|m| f(&m.session)))
        };
        let mut device_ids: Vec<&str> = Vec::new();
        for m in &e.manifests {
            if !m.device.is_empty() && !device_ids.contains(&m.device.as_str()) {
                device_ids.push(&m.device);
            }
        }
        let first = e.manifests.iter().filter_map(|m| m.session.first_ts_ms).min();
        let last = e.manifests.iter().filter_map(|m| m.session.last_ts_ms).max();
        let me = format!("{}:{}", e.agent, e.id);
        SessionSummary {
            title: pick(&|s| s.title.clone()),
            cwd: pick(&|s| s.cwd.clone()),
            agent_version: pick(&|s| s.agent_version.clone()),
            devices: device_ids.into_iter().map(|d| self.device_ref(d)).collect(),
            first_ts_ms: first,
            last_ts_ms: last,
            uploaded_ms: e.manifests.iter().map(|m| m.created_ms).max().unwrap_or(0),
            bytes: head.map(|m| m.bytes()).unwrap_or(0),
            files: head.map(|m| m.files.len()).unwrap_or(0),
            versions: e.manifests.len(),
            heads: heads.len(),
            parent: head
                .and_then(|m| m.parent_session.as_deref())
                .map(|p| normalize_ref(p, &e.agent)),
            children: self.children.get(&me).map(Vec::len).unwrap_or(0),
            agent: e.agent.clone(),
            id: e.id.clone(),
            session_ref: me,
        }
    }
}

/// A parent written as a bare id belongs to the same agent.
fn normalize_ref(text: &str, agent: &str) -> String {
    if text.contains(':') {
        text.to_string()
    } else {
        format!("{agent}:{text}")
    }
}

pub struct Archive {
    provider: Arc<dyn Provider>,
    index: RwLock<Option<(Instant, Arc<Index>)>>,
    refresh_lock: tokio::sync::Mutex<()>,
    ttl: Duration,
    last_error: Mutex<Option<String>>,
    manifests: Mutex<HashMap<String, Arc<Manifest>>>,
    devices: Mutex<HashMap<String, DeviceInfo>>,
    digests: Mutex<HashMap<String, DigestInfo>>,
    pub(crate) linemaps: Mutex<HashMap<String, Arc<LineMap>>>,
    logs: Mutex<HashMap<String, ParsedLog>>,
}

impl Archive {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            provider,
            index: RwLock::new(None),
            refresh_lock: tokio::sync::Mutex::new(()),
            ttl: DEFAULT_TTL,
            last_error: Mutex::new(None),
            manifests: Mutex::default(),
            devices: Mutex::default(),
            digests: Mutex::default(),
            linemaps: Mutex::default(),
            logs: Mutex::default(),
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    /// The error of the last refresh that failed and was answered from an older index.
    pub fn last_refresh_error(&self) -> Option<String> {
        self.last_error.lock().unwrap().clone()
    }

    /// The index, listing the storage again if it is older than the TTL.
    pub async fn index(&self) -> Result<Arc<Index>> {
        if let Some(index) = self.fresh() {
            return Ok(index);
        }
        let _guard = self.refresh_lock.lock().await;
        if let Some(index) = self.fresh() {
            return Ok(index);
        }
        match self.build().await {
            Ok(index) => Ok(self.store(index)),
            Err(error) => {
                // Better an old answer than none while the storage is down.
                let stale = self.index.read().unwrap().as_ref().map(|(_, i)| i.clone());
                match stale {
                    Some(index) => {
                        tracing::warn!(%error, "refresh failed; serving the previous index");
                        *self.last_error.lock().unwrap() = Some(error.to_string());
                        Ok(index)
                    }
                    None => Err(error),
                }
            }
        }
    }

    /// Lists the storage again now.
    pub async fn refresh(&self) -> Result<Arc<Index>> {
        let _guard = self.refresh_lock.lock().await;
        let index = self.build().await?;
        Ok(self.store(index))
    }

    fn fresh(&self) -> Option<Arc<Index>> {
        let guard = self.index.read().unwrap();
        let (at, index) = guard.as_ref()?;
        (at.elapsed() < self.ttl).then(|| index.clone())
    }

    fn store(&self, index: Index) -> Arc<Index> {
        let index = Arc::new(index);
        *self.index.write().unwrap() = Some((Instant::now(), index.clone()));
        *self.last_error.lock().unwrap() = None;
        index
    }

    async fn list_or_empty(&self, dir: &str) -> Result<Vec<Entry>> {
        match self.provider.list(dir).await {
            Ok(entries) => Ok(entries),
            Err(storage::Error::NotFound(_)) => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// A file that never changes: fetched once, then answered from memory.
    async fn cached<T: Clone + Send + Sync + 'static>(
        &self,
        cache: &Mutex<HashMap<String, T>>,
        path: &str,
        parse: impl FnOnce(&[u8]) -> Result<T>,
    ) -> Result<T> {
        if let Some(hit) = cache.lock().unwrap().get(path) {
            return Ok(hit.clone());
        }
        let bytes = self.provider.read(path, None).await?;
        let value = parse(&bytes)?;
        cache.lock().unwrap().insert(path.to_string(), value.clone());
        Ok(value)
    }

    async fn build(&self) -> Result<Index> {
        let mut skipped = 0u64;
        let top = match self.provider.list(layout::ROOT).await {
            Ok(entries) => Some(entries),
            Err(storage::Error::NotFound(_)) => None,
            Err(e) => return Err(e.into()),
        };
        let layout_entries: Vec<String> = top.iter().flatten().map(|e| e.name.clone()).collect();

        // Which sessions exist: the directories under sessions/<agent>/.
        let mut pairs = Vec::new();
        for agent in self
            .list_or_empty(&layout::sessions_dir())
            .await?
            .into_iter()
            .filter(|e| e.is_dir)
        {
            for session in self.list_or_empty(&agent.path).await?.into_iter().filter(|e| e.is_dir) {
                pairs.push((agent.name.clone(), session.name));
            }
        }

        // Their manifests.
        let listings: Vec<_> = stream::iter(pairs)
            .map(|(agent, id)| async move {
                let listing = match layout::manifests_dir(&agent, &id) {
                    Ok(dir) => self.list_or_empty(&dir).await,
                    Err(e) => Err(e),
                };
                (agent, id, listing)
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;
        let mut wanted = Vec::new();
        for (agent, id, listing) in listings {
            match listing {
                Ok(entries) => {
                    for e in entries.into_iter().filter(|e| !e.is_dir && e.name.ends_with(".json")) {
                        wanted.push((agent.clone(), id.clone(), e));
                    }
                }
                Err(Error::Storage(storage::Error::NotFound(_))) => {}
                Err(error) => {
                    tracing::warn!(%error, "cannot list the manifests of a session");
                    skipped += 1;
                }
            }
        }
        let fetched: Vec<_> = stream::iter(wanted)
            .map(|(agent, id, entry)| async move {
                let stem = entry.name.trim_end_matches(".json").to_string();
                let result = self
                    .cached(self.manifests_cache(), &entry.path, |b| {
                        Manifest::from_json(b, &stem, &agent, &id).map(Arc::new)
                    })
                    .await;
                (agent, id, entry.path, result)
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;
        let mut grouped: HashMap<(String, String), Vec<Arc<Manifest>>> = HashMap::new();
        for (agent, id, path, result) in fetched {
            match result {
                Ok(manifest) => grouped.entry((agent, id)).or_default().push(manifest),
                Err(error) => {
                    tracing::warn!(%error, path, "cannot read a manifest");
                    skipped += 1;
                }
            }
        }
        let mut sessions: Vec<SessionEntry> = grouped
            .into_iter()
            .map(|((agent, id), mut manifests)| {
                manifests.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then_with(|| a.id.cmp(&b.id)));
                SessionEntry { agent, id, manifests }
            })
            .collect();
        sessions.sort_by(|a, b| (&a.agent, &a.id).cmp(&(&b.agent, &b.id)));

        let devices = self.load_devices(&sessions, &mut skipped).await?;
        let digests = self.load_digests(&mut skipped).await?;
        let (sessions, by_ref, children) = Index::lookups(sessions);
        Ok(Index {
            refreshed_ms: now_ms(),
            layout_found: top.is_some(),
            layout_entries,
            devices,
            digests,
            skipped,
            sessions,
            by_ref,
            children,
        })
    }

    fn manifests_cache(&self) -> &Mutex<HashMap<String, Arc<Manifest>>> {
        &self.manifests
    }

    async fn load_devices(&self, sessions: &[SessionEntry], skipped: &mut u64) -> Result<Vec<DeviceInfo>> {
        let entries: Vec<Entry> = self
            .list_or_empty(&layout::devices_dir())
            .await?
            .into_iter()
            .filter(|e| !e.is_dir && e.name.ends_with(".json"))
            .collect();
        let results: Vec<_> = stream::iter(entries)
            .map(|entry| async move {
                let stem = entry.name.trim_end_matches(".json").to_string();
                let result = self
                    .cached(&self.devices, &entry.path, |b| {
                        let v: serde_json::Value =
                            serde_json::from_slice(b).map_err(|e| Error::format("device", e.to_string()))?;
                        Ok(DeviceInfo {
                            id: json::string(&v, &["id"]).unwrap_or(stem),
                            name: json::string(&v, &["name"]),
                            first_seen_ms: json::time_field(&v, &["created_at_ms", "created_at", "first_seen_ms"]),
                        })
                    })
                    .await;
                (entry.path, result)
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;
        let mut devices = Vec::new();
        for (path, result) in results {
            match result {
                Ok(d) => devices.push(d),
                Err(error) => {
                    tracing::warn!(%error, path, "cannot read a device file");
                    *skipped += 1;
                }
            }
        }
        // Devices that uploaded but left no device file.
        for s in sessions {
            for m in &s.manifests {
                if !m.device.is_empty() && !devices.iter().any(|d| d.id == m.device) {
                    devices.push(DeviceInfo {
                        id: m.device.clone(),
                        name: None,
                        first_seen_ms: None,
                    });
                }
            }
        }
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(devices)
    }

    async fn load_digests(&self, skipped: &mut u64) -> Result<Vec<DigestInfo>> {
        let entries: Vec<Entry> = self
            .list_or_empty(&layout::digests_dir())
            .await?
            .into_iter()
            .filter(|e| !e.is_dir && e.name.ends_with(".json"))
            .collect();
        let results: Vec<_> = stream::iter(entries)
            .map(|entry| async move {
                let stem = entry.name.trim_end_matches(".json").to_string();
                let result = self
                    .cached(&self.digests, &entry.path, |b| DigestInfo::from_json(b, &stem))
                    .await;
                (entry.path, result)
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;
        let mut out = Vec::new();
        for (path, result) in results {
            match result {
                Ok(d) => out.push(d),
                Err(error) => {
                    tracing::warn!(%error, path, "cannot read a digest");
                    *skipped += 1;
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Sessions that pass the filter, newest activity first, and how many there are in all.
    pub async fn sessions(&self, filter: &SessionFilter) -> Result<(usize, Vec<SessionSummary>)> {
        let index = self.index().await?;
        let q = filter.q.as_deref().map(str::to_lowercase).filter(|q| !q.is_empty());
        let mut out: Vec<SessionSummary> = index
            .sessions
            .iter()
            .filter(|e| filter.agent.as_deref().is_none_or(|a| a == e.agent))
            .filter(|e| {
                filter
                    .device
                    .as_deref()
                    .is_none_or(|d| e.manifests.iter().any(|m| m.device == d))
            })
            .filter(|e| filter.include_children || e.latest_head().is_none_or(|m| m.parent_session.is_none()))
            .map(|e| index.summary(e))
            .filter(|s| {
                q.as_deref().is_none_or(|q| {
                    [Some(&s.id), s.title.as_ref(), s.cwd.as_ref()]
                        .into_iter()
                        .flatten()
                        .any(|text| text.to_lowercase().contains(q))
                })
            })
            .collect();
        out.sort_by(|a, b| {
            let ka = a.last_ts_ms.unwrap_or(a.uploaded_ms);
            let kb = b.last_ts_ms.unwrap_or(b.uploaded_ms);
            kb.cmp(&ka).then_with(|| a.session_ref.cmp(&b.session_ref))
        });
        let total = out.len();
        let page = out.into_iter().skip(filter.offset).take(filter.limit).collect();
        Ok((total, page))
    }

    pub async fn session(&self, agent: &str, id: &str, version: Option<&str>) -> Result<SessionDetail> {
        let index = self.index().await?;
        let entry = index
            .entry(agent, id)
            .ok_or_else(|| Error::NotFound(format!("session {agent}:{id}")))?;
        let manifest = entry.pick(version)?;
        let heads: HashSet<&str> = entry.heads().iter().map(|m| m.id.as_str()).collect();
        let me = format!("{agent}:{id}");
        let children = index
            .children
            .get(&me)
            .map(|ix| ix.iter().map(|i| index.summary(&index.sessions[*i])).collect())
            .unwrap_or_default();
        Ok(SessionDetail {
            summary: index.summary(entry),
            selected_version: manifest.id.clone(),
            versions: entry
                .manifests
                .iter()
                .rev()
                .map(|m| VersionInfo {
                    id: m.id.clone(),
                    parent: m.parent.clone(),
                    device: index.device_ref(&m.device),
                    created_ms: m.created_ms,
                    rewrite: m.rewrite,
                    bytes: m.bytes(),
                    is_head: heads.contains(m.id.as_str()),
                })
                .collect(),
            files: manifest
                .files
                .iter()
                .map(|f| FileInfo {
                    path: f.path.clone(),
                    role: f.role.as_str(),
                    bytes: f.size,
                    lines: f.lines.or_else(|| {
                        f.segments
                            .iter()
                            .map(|s| s.lines)
                            .sum::<Option<u64>>()
                            .filter(|_| !f.segments.is_empty())
                    }),
                    segments: f.segments.len(),
                })
                .collect(),
            children,
            fork_from: manifest.fork_from.as_ref().map(|f: &ForkFrom| ForkInfo {
                session: f.session.clone(),
                byte: f.byte,
            }),
            digests: index
                .digests
                .iter()
                .filter(|d| d.sessions.contains(&me))
                .map(|d| DigestRef {
                    id: d.id.clone(),
                    updated_ms: d.updated_ms,
                })
                .collect(),
        })
    }

    pub async fn devices(&self) -> Result<Vec<DeviceStats>> {
        let index = self.index().await?;
        Ok(index
            .devices
            .iter()
            .map(|d| {
                let mut sessions = 0u64;
                let mut bytes = 0u64;
                let mut last = None;
                for e in &index.sessions {
                    let mine: Vec<&Arc<Manifest>> = e.manifests.iter().filter(|m| m.device == d.id).collect();
                    if let Some(newest) = mine.last() {
                        sessions += 1;
                        bytes += newest.bytes();
                        last = last.max(Some(newest.created_ms));
                    }
                }
                DeviceStats {
                    id: d.id.clone(),
                    name: d.name.clone(),
                    first_seen_ms: d.first_seen_ms,
                    sessions,
                    bytes,
                    last_upload_ms: last,
                }
            })
            .collect())
    }

    /// The merged operation log of every device, newest first, and how many
    /// lines in the log files could not be read.
    pub async fn activity(&self, limit: usize) -> Result<(Vec<LogRecord>, u64)> {
        let mut files = Vec::new();
        for dir in self
            .list_or_empty(&layout::logs_dir())
            .await?
            .into_iter()
            .filter(|e| e.is_dir)
        {
            for file in self
                .list_or_empty(&dir.path)
                .await?
                .into_iter()
                .filter(|e| !e.is_dir && e.name.ends_with(".jsonl"))
            {
                files.push((dir.name.clone(), file.path));
            }
        }
        let results: Vec<_> = stream::iter(files)
            .map(|(device, path)| async move {
                let result = self
                    .cached(&self.logs, &path, |b| Ok(Arc::new(logs::parse(b, &device))))
                    .await;
                (path, result)
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;
        let mut records = Vec::new();
        let mut skipped = 0;
        for (path, result) in results {
            match result {
                Ok(parsed) => {
                    skipped += parsed.1;
                    records.extend(parsed.0.iter().cloned());
                }
                Err(error) => {
                    tracing::warn!(%error, path, "cannot read a log file");
                    skipped += 1;
                }
            }
        }
        records.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| b.seq.cmp(&a.seq)));
        records.truncate(limit);
        Ok((records, skipped))
    }

    /// The text of a digest (digests/<id>.md).
    pub async fn digest_text(&self, id: &str) -> Result<String> {
        let path = format!("{}/{}.md", layout::digests_dir(), layout::component(id)?);
        let bytes = self.provider.read(&path, None).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub(crate) fn role_is_transcript(role: Role) -> bool {
        role == Role::Transcript
    }
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
