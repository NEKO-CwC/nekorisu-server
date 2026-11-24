use std::ops::Range;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use nekorisu_storage::{Capabilities, Entry, Error, Provider, Result, WebDav, WebDavConfig};

use crate::settings::Settings;

/// Stands in when no storage is configured: every read says why it cannot.
/// The device protocol does not need a storage, so the server still runs.
pub struct NotConfigured;

const WHY: &str = "no storage is configured; set NEKORISU_WEBDAV_URL";

#[async_trait]
impl Provider for NotConfigured {
    fn describe(&self) -> String {
        "not configured".to_string()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    async fn list(&self, _dir: &str) -> Result<Vec<Entry>> {
        Err(Error::Other(WHY.into()))
    }

    async fn stat(&self, _path: &str) -> Result<Option<Entry>> {
        Err(Error::Other(WHY.into()))
    }

    async fn read(&self, _path: &str, _range: Option<Range<u64>>) -> Result<Bytes> {
        Err(Error::Other(WHY.into()))
    }
}

pub fn provider(settings: &Settings) -> Result<Arc<dyn Provider>> {
    match &settings.webdav {
        None => Ok(Arc::new(NotConfigured)),
        Some(dav) => {
            let mut config = WebDavConfig::new(&dav.url);
            config.user = dav.user.clone();
            config.password = dav.password.clone();
            Ok(Arc::new(WebDav::new(config)?))
        }
    }
}
