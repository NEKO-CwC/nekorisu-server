//! The device-facing side of the server: the config
//! the user writes in the config directory is checked, signed, stored as
//! numbered revisions, and handed to devices over gRPC. Devices report their
//! state back.
//!
//! A config comes from the config directory, plus what is edited in the panel
//! (path maps, and force requests for path rules devices locked), which is
//! stored here and signed with the rest.

pub mod proto {
    #![allow(clippy::all)]
    tonic::include_proto!("nekorisu.cloud.v1");
}

mod doc;
mod error;
mod paths;
mod publisher;
mod service;
mod signer;
mod source;
mod store;

pub use doc::{ConfigDoc, PathMapDoc, PathsDoc};
pub use error::{Error, Result};
pub use paths::{MAX_PATH_MAPS, MAX_PATH_TEXT, PathMapError, check_path_maps};
pub use publisher::{Publisher, SourceStatus};
pub use service::{CloudService, ServiceOptions, Tls, serve};
pub use signer::{FileSigner, Signed, Signer};
pub use source::{Distribution, PATHS_KEY, Resource, check_forced_rule};
pub use store::{
    DeviceState, Failure, LocalPathMap, LocalValue, PanelForce, RequestState, ResourceMeta, Revision, RevisionMeta,
    Store, View,
};

/// The protocol major version this server speaks (`nekorisu-protocol`).
pub const PROTOCOL: &str = "1";
