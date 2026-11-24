use std::sync::{Arc, RwLock};

use nekorisu_archive::Archive;
use nekorisu_cloud::{Signer, SourceStatus, Store};

use crate::auth::Auth;
use crate::oauth::Oauth;
use crate::settings::Settings;

pub struct AppState {
    pub settings: Settings,
    pub archive: Arc<Archive>,
    pub store: Store,
    pub auth: Auth,
    pub oauth: Oauth,
    pub signer: Arc<dyn Signer>,
    pub source: Arc<RwLock<SourceStatus>>,
}

pub type Shared = Arc<AppState>;
