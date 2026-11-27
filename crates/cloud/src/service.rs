//! The gRPC service devices talk to (nekorisu.cloud.v1, cloud.proto).

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio_stream::Stream;
use tonic::metadata::MetadataMap;
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status};

use crate::PROTOCOL;
use crate::paths::{MAX_PATH_MAPS, MAX_PATH_TEXT};
use crate::proto::cloud_server::{Cloud, CloudServer};
use crate::proto::get_config_response::Result as ConfigResult;
use crate::proto::{
    ConfigFailureReason, GetConfigRequest, GetConfigResponse, GetResourceRequest, NotModified, ReportStateRequest,
    ReportStateResponse, RequestState as ProtoRequestState, ResourceChunk, SignedConfig,
};
use crate::store::{DeviceState, Failure, LocalPathMap, LocalValue, RequestState, Store, View};

const CHUNK: usize = 256 * 1024;
const MAX_VALUES: usize = 200;
const MAX_KEY: usize = 256;
const MAX_VALUE: usize = 8 * 1024;
const MAX_ID: usize = 128;
/// The largest request a device may send: ReportState at its limits fits in this.
const MAX_REQUEST: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct ServiceOptions {
    /// The device token. Without one, nobody gets the account view and nobody can report.
    pub device_token: Option<String>,
    /// Whether a request without a token gets the public view.
    pub public_config: bool,
}

#[derive(Clone)]
pub struct CloudService {
    store: Store,
    token_hash: Option<[u8; 32]>,
    public_config: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Account,
    Public,
}

impl CloudService {
    pub fn new(store: Store, options: ServiceOptions) -> Self {
        Self {
            store,
            token_hash: options
                .device_token
                .filter(|t| !t.is_empty())
                .map(|t| Sha256::digest(t.as_bytes()).into()),
            public_config: options.public_config,
        }
    }

    pub fn into_server(self) -> CloudServer<Self> {
        CloudServer::new(self).max_decoding_message_size(MAX_REQUEST)
    }

    fn token_ok(&self, token: &str) -> bool {
        match &self.token_hash {
            Some(expected) => {
                let given: [u8; 32] = Sha256::digest(token.as_bytes()).into();
                bool::from(given.ct_eq(expected))
            }
            None => false,
        }
    }

    fn access(&self, meta: &MetadataMap) -> Result<Access, Status> {
        check_protocol(meta)?;
        match bearer(meta)? {
            Some(token) if self.token_ok(&token) => Ok(Access::Account),
            Some(_) => Err(Status::unauthenticated("the token is not valid")),
            None if self.public_config => Ok(Access::Public),
            None => Err(Status::unauthenticated("a token is required")),
        }
    }
}

fn check_protocol(meta: &MetadataMap) -> Result<(), Status> {
    match meta.get("nekorisu-protocol").map(|v| v.to_str()) {
        None => Ok(()),
        Some(Ok(v)) if v == PROTOCOL => Ok(()),
        Some(_) => Err(Status::failed_precondition(format!(
            "this server speaks nekorisu-protocol {PROTOCOL}"
        ))),
    }
}

fn bearer(meta: &MetadataMap) -> Result<Option<String>, Status> {
    let Some(value) = meta.get("authorization") else {
        return Ok(None);
    };
    let text = value
        .to_str()
        .map_err(|_| Status::unauthenticated("the authorization header is not text"))?;
    let token = text
        .strip_prefix("Bearer ")
        .or_else(|| text.strip_prefix("bearer "))
        .ok_or_else(|| Status::unauthenticated("the authorization header must be \"Bearer <token>\""))?;
    Ok(Some(token.trim().to_string()))
}

fn plain_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_ID
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn internal(error: impl std::fmt::Display) -> Status {
    tracing::error!(%error, "request failed");
    Status::internal("internal error")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

type ChunkStream = Pin<Box<dyn Stream<Item = Result<ResourceChunk, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl Cloud for CloudService {
    async fn get_config(&self, request: Request<GetConfigRequest>) -> Result<Response<GetConfigResponse>, Status> {
        let access = self.access(request.metadata())?;
        let view = if access == Access::Account {
            View::Account
        } else {
            View::Public
        };
        let held = request.into_inner().etag;
        let revision = self
            .store
            .latest(view)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("no config has been published yet"))?;
        let result = if !held.is_empty() && held == revision.etag {
            ConfigResult::NotModified(NotModified {})
        } else {
            ConfigResult::Config(SignedConfig {
                payload: revision.payload,
                key_id: revision.key_id,
                signature: revision.signature,
            })
        };
        Ok(Response::new(GetConfigResponse {
            result: Some(result),
            etag: revision.etag,
        }))
    }

    type GetResourceStream = ChunkStream;

    async fn get_resource(&self, request: Request<GetResourceRequest>) -> Result<Response<ChunkStream>, Status> {
        let access = self.access(request.metadata())?;
        let view = if access == Access::Account {
            View::Account
        } else {
            View::Public
        };
        let sha = request.into_inner().sha256.to_ascii_lowercase();
        let not_found = || Status::not_found("no such resource");
        // Only what the current config this caller may see points to.
        let revision = self.store.latest(view).await.map_err(internal)?.ok_or_else(not_found)?;
        if !revision.resources.contains(&sha) {
            return Err(not_found());
        }
        let bytes = self
            .store
            .resource_bytes(&sha)
            .await
            .map_err(internal)?
            .ok_or_else(not_found)?;
        let total = bytes.len() as u64;
        let chunks: Vec<Result<ResourceChunk, Status>> = if bytes.is_empty() {
            vec![Ok(ResourceChunk {
                total_size: 0,
                data: Vec::new(),
            })]
        } else {
            bytes
                .chunks(CHUNK)
                .enumerate()
                .map(|(i, c)| {
                    Ok(ResourceChunk {
                        total_size: if i == 0 { total } else { 0 },
                        data: c.to_vec(),
                    })
                })
                .collect()
        };
        Ok(Response::new(Box::pin(tokio_stream::iter(chunks))))
    }

    async fn report_state(
        &self,
        request: Request<ReportStateRequest>,
    ) -> Result<Response<ReportStateResponse>, Status> {
        if self.access(request.metadata())? != Access::Account {
            return Err(Status::unauthenticated("reporting needs a token"));
        }
        let device = request
            .metadata()
            .get("nekorisu-device")
            .and_then(|v| v.to_str().ok())
            .filter(|d| plain_id(d))
            .map(str::to_owned)
            .ok_or_else(|| Status::invalid_argument("nekorisu-device must be set to the device ID"))?;
        let client_version: String = request
            .metadata()
            .get("nekorisu-version")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        let report = request.into_inner();
        let bad = |why: &str| Err(Status::invalid_argument(why.to_string()));
        if report.local_values.len() > MAX_VALUES || report.requests.len() > MAX_VALUES {
            return bad("too many entries");
        }
        if report.applied_etag.len() > MAX_ID {
            return bad("applied_etag is too long");
        }
        if report.paths.len() > MAX_PATH_MAPS {
            return bad("too many path maps");
        }
        let mut paths = Vec::new();
        for p in &report.paths {
            if p.from.is_empty() || p.from.len() > MAX_PATH_TEXT || p.to.len() > MAX_PATH_TEXT {
                return bad("a path map has no `from` or is too long");
            }
            paths.push(LocalPathMap {
                from: p.from.clone(),
                to: p.to.clone(),
                locked: p.locked,
            });
        }
        let mut local_values = Vec::new();
        for v in &report.local_values {
            if v.key.is_empty() || v.key.len() > MAX_KEY || v.value_json.len() > MAX_VALUE {
                return bad("a local value is too large or has no key");
            }
            local_values.push(LocalValue {
                key: v.key.clone(),
                value_json: v.value_json.clone(),
                locked: v.locked,
            });
        }
        let mut requests = Vec::new();
        for r in &report.requests {
            if r.id.is_empty() || r.id.len() > MAX_ID {
                return bad("a request id is empty or too long");
            }
            requests.push(RequestState {
                id: r.id.clone(),
                state: match r.state() {
                    ProtoRequestState::Accepted => "accepted",
                    ProtoRequestState::Rejected => "rejected",
                    ProtoRequestState::Pending | ProtoRequestState::Unspecified => "pending",
                }
                .to_string(),
            });
        }
        let last_failure = report.last_failure.as_ref().map(|f| Failure {
            serial: f.serial,
            reason: match f.reason() {
                ConfigFailureReason::BadSignature => "bad_signature",
                ConfigFailureReason::UnsupportedSchema => "unsupported_schema",
                ConfigFailureReason::ResourceUnavailable => "resource_unavailable",
                ConfigFailureReason::ResourceHashMismatch => "resource_hash_mismatch",
                ConfigFailureReason::Other | ConfigFailureReason::Unspecified => "other",
            }
            .to_string(),
        });
        let state = DeviceState {
            received_ms: now_ms(),
            client_version,
            applied_serial: report.applied_serial,
            applied_etag: report.applied_etag,
            exec_allowed: report.exec_allowed,
            local_values,
            requests,
            last_failure,
            paths,
        };
        self.store.upsert_report(&device, &state).await.map_err(internal)?;
        Ok(Response::new(ReportStateResponse {}))
    }
}

/// A TLS certificate and key, PEM.
pub struct Tls {
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
}

/// Serves the device protocol on `addr` until `shutdown` finishes. Without
/// `tls` it speaks plain HTTP/2, for use behind a reverse proxy.
pub async fn serve(
    addr: SocketAddr,
    service: CloudService,
    tls: Option<Tls>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), tonic::transport::Error> {
    let mut builder = Server::builder();
    if let Some(tls) = tls {
        builder = builder.tls_config(ServerTlsConfig::new().identity(Identity::from_pem(tls.cert_pem, tls.key_pem)))?;
    }
    builder
        .concurrency_limit_per_connection(32)
        .add_service(service.into_server())
        .serve_with_shutdown(addr, shutdown)
        .await
}
