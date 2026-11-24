//! The archive layer: reads the remote layout nekorisu devices write
//! through
//! a storage provider, and answers questions about it: which sessions exist,
//! what versions they have, what their lines say.
//!
//! Read-only. Remote files are written once and never change, so everything
//! fetched is cached by path for good; a refresh only lists directories and
//! fetches the files it has not seen. Session content is never stored: lines
//! are read by byte range at the moment they are asked for.
//!
//! The JSON field names of manifests, line maps, logs and digests are
//! provisional: readers accept several spellings and
//! ignore what they do not know.

mod error;
mod index;
mod json;
mod layout;
mod linemap;
mod lines;
mod logs;
mod model;

pub use error::{Error, Result};
pub use index::{Archive, DeviceInfo, DeviceStats, Index, SessionDetail, SessionFilter, SessionSummary, VersionInfo};
pub use layout::ROOT;
pub use linemap::{Kind, LineEntry, LineMap};
pub use lines::{LineRecord, LinesPage, LinesQuery, Selector};
pub use logs::LogRecord;
pub use model::{DigestInfo, ForkFrom, Manifest, ManifestFile, Role, SegmentRef, SessionMeta, SessionRef};
