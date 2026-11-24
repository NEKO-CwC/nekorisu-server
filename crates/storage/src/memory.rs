use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bytes::Bytes;

use crate::{Capabilities, Entry, Error, Provider, Result, clean, join};

/// Files in memory. For tests and tools: it is not a storage a user can pick.
#[derive(Default)]
pub struct MemoryProvider {
    files: RwLock<BTreeMap<String, Bytes>>,
    reads: AtomicU64,
    bytes_read: AtomicU64,
}

impl MemoryProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces a file. Panics on a path `clean` refuses: this is test setup.
    pub fn put(&self, path: &str, body: impl Into<Bytes>) {
        let path = clean(path).expect("valid path");
        self.files.write().unwrap().insert(path, body.into());
    }

    /// Removes a file; `true` if it was there.
    pub fn remove(&self, path: &str) -> bool {
        let path = clean(path).expect("valid path");
        self.files.write().unwrap().remove(&path).is_some()
    }

    /// How many `read` calls were made, and how many bytes they returned.
    pub fn read_stats(&self) -> (u64, u64) {
        (
            self.reads.load(Ordering::Relaxed),
            self.bytes_read.load(Ordering::Relaxed),
        )
    }
}

#[async_trait]
impl Provider for MemoryProvider {
    fn describe(&self) -> String {
        "memory".to_string()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { range_read: Some(true) }
    }

    async fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let dir = clean(dir)?;
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let files = self.files.read().unwrap();
        let mut dirs = BTreeSet::new();
        let mut out = Vec::new();
        let mut found = dir.is_empty();
        for (path, body) in files.iter() {
            let Some(rest) = path.strip_prefix(&prefix) else {
                continue;
            };
            found = true;
            match rest.split_once('/') {
                Some((first, _)) => {
                    dirs.insert(first.to_string());
                }
                None => out.push(Entry {
                    name: rest.to_string(),
                    path: path.clone(),
                    is_dir: false,
                    size: body.len() as u64,
                    modified_ms: None,
                }),
            }
        }
        if !found {
            return Err(Error::NotFound(dir));
        }
        out.extend(dirs.into_iter().map(|name| Entry {
            path: join(&dir, &name),
            name,
            is_dir: true,
            size: 0,
            modified_ms: None,
        }));
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn stat(&self, path: &str) -> Result<Option<Entry>> {
        let path = clean(path)?;
        let files = self.files.read().unwrap();
        let name = path.rsplit('/').next().unwrap_or_default().to_string();
        if let Some(body) = files.get(&path) {
            return Ok(Some(Entry {
                name,
                path,
                is_dir: false,
                size: body.len() as u64,
                modified_ms: None,
            }));
        }
        let prefix = format!("{path}/");
        if path.is_empty() || files.keys().any(|k| k.starts_with(&prefix)) {
            return Ok(Some(Entry {
                name,
                path,
                is_dir: true,
                size: 0,
                modified_ms: None,
            }));
        }
        Ok(None)
    }

    async fn read(&self, path: &str, range: Option<Range<u64>>) -> Result<Bytes> {
        let path = clean(path)?;
        let body = self
            .files
            .read()
            .unwrap()
            .get(&path)
            .cloned()
            .ok_or_else(|| Error::NotFound(path.clone()))?;
        let out = match range {
            None => body,
            Some(r) => {
                let len = body.len() as u64;
                let start = r.start.min(len) as usize;
                let end = r.end.min(len).max(start as u64) as usize;
                body.slice(start..end)
            }
        };
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.bytes_read.fetch_add(out.len() as u64, Ordering::Relaxed);
        Ok(out)
    }
}
