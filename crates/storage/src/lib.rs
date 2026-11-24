//! The storage layer: a small file interface over the place where nekorisu
//! devices keep their sessions.
//!
//! A provider knows nothing about the archive layout. It lists, stats and
//! reads files, nothing else: the first version of the server never writes to
//! the user's storage, so there is no write call yet. Only WebDAV is
//! implemented.

mod error;
mod memory;
mod path;
mod webdav;

use std::ops::Range;

use async_trait::async_trait;
use bytes::Bytes;

pub use error::{Error, Result};
pub use memory::MemoryProvider;
pub use path::{clean, join};
pub use webdav::{WebDav, WebDavConfig};

/// One file or directory in a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The last path segment.
    pub name: String,
    /// Path from the storage root, no leading or trailing slash.
    pub path: String,
    pub is_dir: bool,
    /// Bytes; 0 for a directory or when the server does not say.
    pub size: u64,
    pub modified_ms: Option<i64>,
}

/// What the storage turned out to support. Learned by using it, not assumed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Does the server answer a byte-range request with only those bytes?
    /// `None` until a range read has been tried. `Some(false)` means a line
    /// read downloads the whole file.
    pub range_read: Option<bool>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// A short description for the panel. Never contains credentials.
    fn describe(&self) -> String;

    fn capabilities(&self) -> Capabilities;

    /// One level, not recursive. `NotFound` when the directory does not exist.
    async fn list(&self, dir: &str) -> Result<Vec<Entry>>;

    /// `None` when nothing is at `path`.
    async fn stat(&self, path: &str) -> Result<Option<Entry>>;

    /// The whole file, or the bytes in `range` (clamped to the end of the file).
    async fn read(&self, path: &str, range: Option<Range<u64>>) -> Result<Bytes>;
}
