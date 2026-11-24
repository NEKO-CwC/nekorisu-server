//! A real gRPC client against the real service on a loopback port, with the
//! client library's own `sign` module checking what comes back.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use nekorisu::sign::{CONTEXT_CONFIG, Envelope, PublicKey};
use nekorisu_cloud::proto::cloud_client::CloudClient;
use nekorisu_cloud::proto::get_config_response::Result as ConfigResult;
use nekorisu_cloud::proto::{
    ConfigFailure, ConfigFailureReason, GetConfigRequest, GetResourceRequest, LocalValue, ReportStateRequest,
    RequestState, RequestStatus,
};
use nekorisu_cloud::{CloudService, ConfigDoc, FileSigner, Publisher, ServiceOptions, Signer, Store, View};
use sqlx::sqlite::SqlitePoolOptions;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use tonic::{Code, Request};

const TOKEN: &str = "device-token-for-tests";

const CONFIG: &str = r#"
min_client_version = "0.1.0"
poll_interval_secs = 600

[digest.excerpt]
name = "excerpt-default"
version = "2026-11-23.1"
file = "resources/excerpt.json"

[digest.summary]
name = "summary"
version = "1"
file = "resources/summary.sh"
executable = true

[[force]]
id = "f-1"
devices = ["dev-1"]
[[force.changes]]
key = "local.copy_form"
value = "full"

[[requests]]
id = "r-1"
action = "allow"
devices = ["dev-1"]
"#;

struct Harness {
    dir: tempfile::TempDir,
    store: Store,
    signer: Arc<FileSigner>,
    publisher: Publisher,
}

async fn harness() -> Harness {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let store = Store::new(pool);
    store.migrate().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("resources")).unwrap();
    std::fs::write(dir.path().join("resources/excerpt.json"), r#"{"runner":"excerpt"}"#).unwrap();
    std::fs::write(dir.path().join("resources/summary.sh"), "echo hi").unwrap();
    std::fs::write(dir.path().join("distribution.toml"), CONFIG).unwrap();
    let signer = Arc::new(FileSigner::generate());
    let publisher = Publisher::new(dir.path().to_path_buf(), store.clone(), signer.clone());
    Harness {
        dir,
        store,
        signer,
        publisher,
    }
}

async fn start(store: &Store, public_config: bool) -> CloudClient<Channel> {
    let service = CloudService::new(
        store.clone(),
        ServiceOptions {
            device_token: Some(TOKEN.to_string()),
            public_config,
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(service.into_server())
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    let channel = Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .unwrap();
    CloudClient::new(channel)
}

fn request<T>(message: T, token: Option<&str>, device: Option<&str>) -> Request<T> {
    let mut r = Request::new(message);
    r.metadata_mut().insert("nekorisu-protocol", "1".parse().unwrap());
    r.metadata_mut().insert("nekorisu-version", "0.1.0".parse().unwrap());
    if let Some(device) = device {
        r.metadata_mut()
            .insert("nekorisu-device", MetadataValue::try_from(device).unwrap());
    }
    if let Some(token) = token {
        r.metadata_mut().insert(
            "authorization",
            MetadataValue::try_from(format!("Bearer {token}")).unwrap(),
        );
    }
    r
}

fn verify(signer: &FileSigner, config: &nekorisu_cloud::proto::SignedConfig) -> ConfigDoc {
    let envelope = Envelope {
        payload: B64.encode(&config.payload),
        key_id: config.key_id.clone(),
        signature: B64.encode(&config.signature),
    };
    let key = PublicKey::from_base64(&signer.public_key()).unwrap();
    let payload = envelope.open(CONFIG_CONTEXT, &[key]).expect("the signature verifies");
    serde_json::from_slice(&payload).unwrap()
}

const CONFIG_CONTEXT: &str = CONTEXT_CONFIG;

#[tokio::test]
async fn publishes_pairs_of_revisions_and_signs_them_for_the_client_library() {
    let h = harness().await;
    assert!(h.publisher.check().await.unwrap(), "first check publishes");
    assert!(
        !h.publisher.check().await.unwrap(),
        "nothing changed, nothing published"
    );

    let public = h.store.latest(View::Public).await.unwrap().unwrap();
    let account = h.store.latest(View::Account).await.unwrap().unwrap();
    assert_eq!((public.serial, account.serial), (1, 2));

    let doc: ConfigDoc = serde_json::from_slice(&account.payload).unwrap();
    assert_eq!(doc.serial, 2);
    assert_eq!(doc.poll_interval_secs, 600);
    assert_eq!(doc.force.len(), 1);
    assert!(doc.digest.summary.as_ref().unwrap().executable);
    let public_doc: ConfigDoc = serde_json::from_slice(&public.payload).unwrap();
    assert_eq!(public_doc.serial, 1);
    assert!(public_doc.force.is_empty() && public_doc.requests.is_empty());

    // A change publishes the next pair.
    std::fs::write(h.dir.path().join("distribution.toml"), CONFIG.replace("600", "1200")).unwrap();
    assert!(h.publisher.check().await.unwrap());
    assert_eq!(h.store.latest(View::Account).await.unwrap().unwrap().serial, 4);
    assert_eq!(h.store.history(50).await.unwrap().len(), 4);
}

#[tokio::test]
async fn a_bad_config_keeps_the_last_good_one_and_says_why() {
    let h = harness().await;
    h.publisher.check().await.unwrap();
    std::fs::write(h.dir.path().join("distribution.toml"), "poll_interval_secs = 5").unwrap();
    assert!(h.publisher.check().await.is_err());
    let status = h.publisher.status().read().unwrap().clone();
    assert!(status.last_error.unwrap().contains("poll_interval_secs"));
    assert_eq!(
        h.store.latest(View::Account).await.unwrap().unwrap().serial,
        2,
        "still the good one"
    );
    // Fixing it clears the error and publishes.
    std::fs::write(h.dir.path().join("distribution.toml"), CONFIG).unwrap();
    assert!(
        !h.publisher.check().await.unwrap(),
        "back to the config that is already published"
    );
    assert!(h.publisher.status().read().unwrap().last_error.is_none());
}

#[tokio::test]
async fn no_config_file_publishes_the_defaults_and_removing_it_later_goes_back_to_them() {
    let h = harness().await;
    std::fs::remove_file(h.dir.path().join("distribution.toml")).unwrap();
    assert!(h.publisher.check().await.unwrap());
    assert!(!h.publisher.status().read().unwrap().configured);
    let doc: ConfigDoc =
        serde_json::from_slice(&h.store.latest(View::Account).await.unwrap().unwrap().payload).unwrap();
    assert_eq!(doc.schema, "1.0");
    assert!(doc.force.is_empty());
    std::fs::write(h.dir.path().join("distribution.toml"), CONFIG).unwrap();
    assert!(h.publisher.check().await.unwrap());
    std::fs::remove_file(h.dir.path().join("distribution.toml")).unwrap();
    assert!(
        h.publisher.check().await.unwrap(),
        "back to the defaults is a new revision"
    );
}

#[tokio::test]
async fn a_new_signing_key_republishes() {
    let h = harness().await;
    h.publisher.check().await.unwrap();
    let other = Arc::new(FileSigner::generate());
    let publisher = Publisher::new(h.dir.path().to_path_buf(), h.store.clone(), other.clone());
    assert!(publisher.check().await.unwrap());
    let account = h.store.latest(View::Account).await.unwrap().unwrap();
    assert_eq!(account.key_id, other.key_id());
}

#[tokio::test]
async fn get_config_with_a_token() {
    let h = harness().await;
    h.publisher.check().await.unwrap();
    let mut client = start(&h.store, false).await;

    let got = client
        .get_config(request(GetConfigRequest::default(), Some(TOKEN), None))
        .await
        .unwrap()
        .into_inner();
    let ConfigResult::Config(signed) = got.result.unwrap() else {
        panic!("a config")
    };
    let doc = verify(&h.signer, &signed);
    assert_eq!(doc.serial, 2);
    assert_eq!(doc.force.len(), 1, "the account view carries the requests");
    assert_eq!(got.etag.len(), 64);

    // With the etag it holds, nothing new.
    let again = client
        .get_config(request(GetConfigRequest { etag: got.etag.clone() }, Some(TOKEN), None))
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(again.result, Some(ConfigResult::NotModified(_))));
    assert_eq!(again.etag, got.etag);

    // A stale etag gets the config again.
    let stale = client
        .get_config(request(GetConfigRequest { etag: "old".into() }, Some(TOKEN), None))
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(stale.result, Some(ConfigResult::Config(_))));
}

#[tokio::test]
async fn tokens_the_public_view_and_protocol_versions() {
    let h = harness().await;
    h.publisher.check().await.unwrap();

    // Public view off: no token, no config; a wrong token never works.
    let mut client = start(&h.store, false).await;
    let code = |r: Result<_, tonic::Status>| r.err().map(|s| s.code());
    assert_eq!(
        code(
            client
                .get_config(request(GetConfigRequest::default(), None, None))
                .await
        ),
        Some(Code::Unauthenticated)
    );
    assert_eq!(
        code(
            client
                .get_config(request(GetConfigRequest::default(), Some("wrong"), None))
                .await
        ),
        Some(Code::Unauthenticated)
    );

    // Public view on: no token gets the public document; a wrong token still fails.
    let mut open = start(&h.store, true).await;
    let got = open
        .get_config(request(GetConfigRequest::default(), None, None))
        .await
        .unwrap()
        .into_inner();
    let ConfigResult::Config(signed) = got.result.unwrap() else {
        panic!("a config")
    };
    let doc = verify(&h.signer, &signed);
    assert_eq!(doc.serial, 1);
    assert!(doc.force.is_empty());
    assert_eq!(
        code(
            open.get_config(request(GetConfigRequest::default(), Some("wrong"), None))
                .await
        ),
        Some(Code::Unauthenticated)
    );

    // A protocol the server does not speak.
    let mut r = request(GetConfigRequest::default(), Some(TOKEN), None);
    r.metadata_mut().insert("nekorisu-protocol", "2".parse().unwrap());
    assert_eq!(code(client.get_config(r).await), Some(Code::FailedPrecondition));
}

#[tokio::test]
async fn no_token_configured_means_no_account_view() {
    let h = harness().await;
    h.publisher.check().await.unwrap();
    let service = CloudService::new(
        h.store.clone(),
        ServiceOptions {
            device_token: None,
            public_config: false,
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(service.into_server())
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    let mut client = CloudClient::connect(format!("http://{addr}")).await.unwrap();
    // even an empty token does not open it
    let err = client
        .get_config(request(GetConfigRequest::default(), Some(""), None))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn resources_come_in_chunks_and_only_to_those_who_may_see_them() {
    let h = harness().await;
    // A big resource, so there is more than one chunk.
    let big = "x".repeat(600 * 1024);
    std::fs::write(h.dir.path().join("resources/big.json"), &big).unwrap();
    std::fs::write(
        h.dir.path().join("distribution.toml"),
        format!("{CONFIG}\n[kind_rules]\nname = \"kinds\"\nversion = \"1\"\nfile = \"resources/big.json\"\n"),
    )
    .unwrap();
    h.publisher.check().await.unwrap();
    let account = h.store.latest(View::Account).await.unwrap().unwrap();
    let doc: ConfigDoc = serde_json::from_slice(&account.payload).unwrap();
    let sha = doc.kind_rules.unwrap().sha256;

    let mut client = start(&h.store, false).await;
    let mut stream = client
        .get_resource(request(GetResourceRequest { sha256: sha.clone() }, Some(TOKEN), None))
        .await
        .unwrap()
        .into_inner();
    let mut bytes = Vec::new();
    let mut chunks = 0;
    let mut first_total = None;
    while let Some(chunk) = stream.message().await.unwrap() {
        if chunks == 0 {
            first_total = Some(chunk.total_size);
        } else {
            assert_eq!(chunk.total_size, 0, "only the first chunk says the size");
        }
        assert!(chunk.data.len() <= 256 * 1024);
        bytes.extend(chunk.data);
        chunks += 1;
    }
    assert!(chunks >= 3);
    assert_eq!(first_total, Some(big.len() as u64));
    assert_eq!(bytes, big.as_bytes());
    assert_eq!(hex::encode(sha2::Sha256::digest(&bytes)), sha);

    // Unknown, and not-in-the-config, look the same.
    for wanted in ["00".repeat(32), "not hex".to_string()] {
        let err = client
            .get_resource(request(GetResourceRequest { sha256: wanted }, Some(TOKEN), None))
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::NotFound);
    }
    // No token, public view off.
    let err = client
        .get_resource(request(GetResourceRequest { sha256: sha.clone() }, None, None))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated);

    // The public view serves what the public config points to: these are the same resources here.
    let mut open = start(&h.store, true).await;
    assert!(
        open.get_resource(request(GetResourceRequest { sha256: sha }, None, None))
            .await
            .is_ok()
    );
}

use sha2::Digest;

#[tokio::test]
async fn devices_report_their_state() {
    let h = harness().await;
    h.publisher.check().await.unwrap();
    let mut client = start(&h.store, false).await;
    let report = ReportStateRequest {
        applied_serial: 2,
        applied_etag: "abc".into(),
        local_values: vec![LocalValue {
            key: "local.copy_form".into(),
            value_json: "\"full\"".into(),
            locked: true,
        }],
        exec_allowed: true,
        requests: vec![
            RequestStatus {
                id: "f-1".into(),
                state: RequestState::Pending as i32,
            },
            RequestStatus {
                id: "r-1".into(),
                state: RequestState::Accepted as i32,
            },
        ],
        last_failure: Some(ConfigFailure {
            serial: 4,
            reason: ConfigFailureReason::ResourceHashMismatch as i32,
        }),
    };
    client
        .report_state(request(report, Some(TOKEN), Some("dev-1")))
        .await
        .unwrap();
    let states = h.store.device_states().await.unwrap();
    assert_eq!(states.len(), 1);
    let (id, state) = &states[0];
    assert_eq!(id, "dev-1");
    assert_eq!(state.client_version, "0.1.0");
    assert_eq!(state.applied_serial, 2);
    assert!(state.exec_allowed);
    assert_eq!(state.local_values[0].key, "local.copy_form");
    assert!(state.local_values[0].locked);
    assert_eq!(state.requests[0].state, "pending");
    assert_eq!(state.requests[1].state, "accepted");
    assert_eq!(state.last_failure.as_ref().unwrap().reason, "resource_hash_mismatch");

    // A second report replaces the first.
    client
        .report_state(request(
            ReportStateRequest {
                applied_serial: 4,
                ..Default::default()
            },
            Some(TOKEN),
            Some("dev-1"),
        ))
        .await
        .unwrap();
    let states = h.store.device_states().await.unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].1.applied_serial, 4);
    assert!(states[0].1.local_values.is_empty());
}

#[tokio::test]
async fn reports_are_authenticated_identified_and_bounded() {
    let h = harness().await;
    h.publisher.check().await.unwrap();
    let mut client = start(&h.store, true).await; // even with the public view on, reporting needs the token
    let report = ReportStateRequest::default;
    let code = |r: Result<_, tonic::Status>| r.err().map(|s| s.code());

    assert_eq!(
        code(client.report_state(request(report(), None, Some("dev-1"))).await),
        Some(Code::Unauthenticated)
    );
    assert_eq!(
        code(client.report_state(request(report(), Some(TOKEN), None)).await),
        Some(Code::InvalidArgument),
        "no device ID"
    );
    assert_eq!(
        code(client.report_state(request(report(), Some(TOKEN), Some("a b/c"))).await),
        Some(Code::InvalidArgument),
        "not a plain ID"
    );

    let many = ReportStateRequest {
        local_values: (0..201)
            .map(|i| LocalValue {
                key: format!("k{i}"),
                value_json: "1".into(),
                locked: false,
            })
            .collect(),
        ..Default::default()
    };
    assert_eq!(
        code(client.report_state(request(many, Some(TOKEN), Some("dev-1"))).await),
        Some(Code::InvalidArgument)
    );
    let huge = ReportStateRequest {
        local_values: vec![LocalValue {
            key: "k".into(),
            value_json: "x".repeat(9000),
            locked: false,
        }],
        ..Default::default()
    };
    assert_eq!(
        code(client.report_state(request(huge, Some(TOKEN), Some("dev-1"))).await),
        Some(Code::InvalidArgument)
    );
    assert!(
        h.store.device_states().await.unwrap().is_empty(),
        "nothing refused was stored"
    );
}
