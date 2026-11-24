//! The device-facing side of the server: the config
//! the user writes in the config directory is checked, signed, stored as
//! numbered revisions, and handed to devices over gRPC. Devices report their
//! state back.
//!
//! Nothing here edits anything on request: the config directory is the only
//! place a config comes from.

pub mod proto {
    #![allow(clippy::all)]
    tonic::include_proto!("nekorisu.cloud.v1");
}

mod doc;
mod error;
mod publisher;
mod service;
mod signer;
mod source;
mod store;

pub use doc::ConfigDoc;
pub use error::{Error, Result};
pub use publisher::{Publisher, SourceStatus};
pub use service::{CloudService, ServiceOptions, Tls, serve};
pub use signer::{FileSigner, Signed, Signer};
pub use source::{Distribution, Resource};
pub use store::{DeviceState, Failure, LocalValue, RequestState, ResourceMeta, Revision, RevisionMeta, Store, View};

/// The protocol major version this server speaks (`nekorisu-protocol`).
pub const PROTOCOL: &str = "1";
