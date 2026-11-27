use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::signer::Signer;
use crate::source::Distribution;
use crate::store::Store;
use crate::{Error, Result};

/// What the panel shows about the config directory.
#[derive(Clone, Debug, Default)]
pub struct SourceStatus {
    pub dir: String,
    /// Whether the directory has a `distribution.toml`. Without one, devices
    /// get the defaults.
    pub configured: bool,
    pub checked_ms: i64,
    /// Why the directory was not published, if it was not. The revision before it stays in force.
    pub last_error: Option<String>,
}

/// Watches the config directory and publishes a new revision when it, or the
/// path maps edited in the panel, change.
pub struct Publisher {
    dir: PathBuf,
    store: Store,
    signer: Arc<dyn Signer>,
    status: Arc<RwLock<SourceStatus>>,
    /// One check at a time: the timer and a panel edit could otherwise both
    /// publish the same change.
    checking: tokio::sync::Mutex<()>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Publisher {
    pub fn new(dir: PathBuf, store: Store, signer: Arc<dyn Signer>) -> Self {
        let status = SourceStatus {
            dir: dir.display().to_string(),
            ..SourceStatus::default()
        };
        Self {
            dir,
            store,
            signer,
            status: Arc::new(RwLock::new(status)),
            checking: tokio::sync::Mutex::new(()),
        }
    }

    pub fn status(&self) -> Arc<RwLock<SourceStatus>> {
        self.status.clone()
    }

    /// Reads the directory and the edited path maps; publishes if they, or the
    /// signing key, are not what the newest revision was made from. `true` if
    /// it published.
    pub async fn check(&self) -> Result<bool> {
        let _one_at_a_time = self.checking.lock().await;
        let maps = self.store.path_maps().await?.0;
        let force: Vec<_> = self.store.panel_force().await?.iter().map(|f| f.to_doc()).collect();
        let dir = self.dir.clone();
        let loaded = tokio::task::spawn_blocking(move || Distribution::load(&dir))
            .await
            .map_err(|e| Error::config(format!("config reader stopped: {e}")))?
            .and_then(|found| {
                let configured = found.is_some();
                let dist = found.unwrap_or_else(Distribution::empty).with_panel(maps, force)?;
                Ok((configured, dist))
            });
        match loaded {
            Ok((configured, dist)) => {
                let wanted = format!("{}:{}", dist.source_hash, self.signer.key_id());
                let current = self.store.newest().await?.map(|r| r.source_hash);
                let published = if current.as_deref() == Some(wanted.as_str()) {
                    false
                } else {
                    let (account, public) = self.store.publish(&dist, self.signer.as_ref(), now_ms()).await?;
                    tracing::info!(
                        serial = account.serial,
                        public_serial = public.serial,
                        "published a config revision"
                    );
                    true
                };
                self.set(configured, None);
                Ok(published)
            }
            Err(error) => {
                tracing::error!(%error, "the config directory was not published; the last good config stays in force");
                self.set(true, Some(error.to_string()));
                Err(error)
            }
        }
    }

    fn set(&self, configured: bool, last_error: Option<String>) {
        let mut status = self.status.write().unwrap();
        status.configured = configured;
        status.checked_ms = now_ms();
        status.last_error = last_error;
    }

    /// Checks now and then every `every`, until the process ends.
    pub fn spawn(self: Arc<Self>, every: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                // Errors are already logged and shown in the status.
                let _ = self.check().await;
                tokio::time::sleep(every).await;
            }
        })
    }
}
