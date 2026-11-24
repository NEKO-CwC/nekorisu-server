//! The provider contract, run against the in-memory provider and against a
//! real WebDAV server (the `dav-server` library, in process), plus the HTTP
//! behaviours worth checking: Range, redirects.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use bytes::Bytes;
use dav_server::{DavHandler, fakels::FakeLs, localfs::LocalFs};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use nekorisu_storage::{Error, MemoryProvider, Provider, WebDav, WebDavConfig};
use tokio::net::TcpListener;

const SEGMENT: &str = "0123456789abcdef";
const FILES: [(&str, &str); 3] = [
    ("nekorisu/v1/devices/d1.json", r#"{"id":"d1"}"#),
    ("nekorisu/v1/sessions/claude-code/s1/files/a.jsonl/0-aaa.seg", SEGMENT),
    ("nekorisu/v1/odd/weird name é#1.json", "{}"),
];

fn write_fixture(root: &Path) {
    for (path, body) in FILES {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
}

/// Serves `root` over WebDAV. With `auth`, requests need that Basic header.
async fn dav_server(root: &Path, auth: Option<&'static str>) -> SocketAddr {
    let handler = DavHandler::builder()
        .filesystem(LocalFs::new(root, false, false, false))
        .locksystem(FakeLs::new())
        .build_handler();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let handler = handler.clone();
            tokio::spawn(async move {
                let service = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let handler = handler.clone();
                    async move {
                        if let Some(expected) = auth {
                            let ok = req
                                .headers()
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .is_some_and(|v| v == expected);
                            if !ok {
                                let mut denied = hyper::Response::new(dav_server::body::Body::empty());
                                *denied.status_mut() = hyper::StatusCode::UNAUTHORIZED;
                                return Ok::<_, Infallible>(denied);
                            }
                        }
                        Ok::<_, Infallible>(handler.handle(req).await)
                    }
                });
                let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    addr
}

async fn contract(p: &dyn Provider) {
    // list
    let root = p.list("").await.unwrap();
    assert!(root.iter().any(|e| e.name == "nekorisu" && e.is_dir));
    let devices = p.list("nekorisu/v1/devices").await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "d1.json");
    assert_eq!(devices[0].path, "nekorisu/v1/devices/d1.json");
    assert!(!devices[0].is_dir);
    assert_eq!(devices[0].size, FILES[0].1.len() as u64);
    let sessions = p.list("/nekorisu/v1/sessions/").await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].is_dir);
    assert!(matches!(p.list("nekorisu/nope").await, Err(Error::NotFound(_))));

    // odd names survive the round trip
    let odd = p.list("nekorisu/v1/odd").await.unwrap();
    assert_eq!(odd.len(), 1);
    assert_eq!(odd[0].name, "weird name é#1.json");
    assert_eq!(p.read(&odd[0].path, None).await.unwrap(), Bytes::from_static(b"{}"));

    // stat
    let file = p.stat("nekorisu/v1/devices/d1.json").await.unwrap().unwrap();
    assert!(!file.is_dir);
    assert_eq!(file.size, FILES[0].1.len() as u64);
    assert!(p.stat("nekorisu/v1/devices").await.unwrap().unwrap().is_dir);
    assert!(p.stat("nekorisu/v1/missing.json").await.unwrap().is_none());

    // read
    let seg = "nekorisu/v1/sessions/claude-code/s1/files/a.jsonl/0-aaa.seg";
    assert_eq!(p.read(seg, None).await.unwrap(), SEGMENT.as_bytes());
    assert_eq!(p.read(seg, Some(2..6)).await.unwrap(), Bytes::from_static(b"2345"));
    assert_eq!(p.read(seg, Some(10..100)).await.unwrap(), Bytes::from_static(b"abcdef"));
    assert!(p.read(seg, Some(50..60)).await.unwrap().is_empty());
    assert!(p.read(seg, Some(3..3)).await.unwrap().is_empty());
    assert!(matches!(p.read("nekorisu/none", None).await, Err(Error::NotFound(_))));
    assert!(matches!(
        p.read("../etc/passwd", None).await,
        Err(Error::InvalidPath(_))
    ));
}

#[tokio::test]
async fn memory_provider_meets_the_contract() {
    let memory = MemoryProvider::new();
    for (path, body) in FILES {
        memory.put(path, body.as_bytes().to_vec());
    }
    contract(&memory).await;
}

#[tokio::test]
async fn webdav_meets_the_contract_and_learns_range_support() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());
    let addr = dav_server(dir.path(), None).await;
    let dav = WebDav::new(WebDavConfig::new(format!("http://{addr}/"))).unwrap();
    assert_eq!(dav.capabilities().range_read, None);
    contract(&dav).await;
    assert_eq!(dav.capabilities().range_read, Some(true));
}

#[tokio::test]
async fn webdav_under_a_sub_path_with_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("files");
    std::fs::create_dir_all(&nested).unwrap();
    write_fixture(&nested);
    // Basic dXNlcjpwdw== is user:pw
    let addr = dav_server(dir.path(), Some("Basic dXNlcjpwdw==")).await;
    let url = format!("http://{addr}/files");

    let wrong = WebDav::new(WebDavConfig {
        user: Some("user".into()),
        password: Some("nope".into()),
        ..WebDavConfig::new(&url)
    })
    .unwrap();
    assert!(matches!(wrong.list("").await, Err(Error::Unauthorized)));

    let right = WebDav::new(WebDavConfig {
        user: Some("user".into()),
        password: Some("pw".into()),
        ..WebDavConfig::new(&url)
    })
    .unwrap();
    contract(&right).await;
}

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<HeaderMap>>>);

async fn whole_file(State(seen): State<Seen>, headers: HeaderMap) -> impl IntoResponse {
    seen.0.lock().unwrap().push(headers);
    (StatusCode::OK, SEGMENT) // ignores Range, like a server that does not support it
}

async fn serve_router(router: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

#[tokio::test]
async fn a_server_that_ignores_range_is_detected_and_still_answers_correctly() {
    let seen = Seen::default();
    let addr = serve_router(Router::new().route("/f", get(whole_file)).with_state(seen)).await;
    let dav = WebDav::new(WebDavConfig::new(format!("http://{addr}/"))).unwrap();
    assert_eq!(dav.read("f", Some(2..6)).await.unwrap(), Bytes::from_static(b"2345"));
    assert_eq!(dav.capabilities().range_read, Some(false));
}

async fn target(State(seen): State<Seen>, headers: HeaderMap) -> impl IntoResponse {
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    seen.0.lock().unwrap().push(headers);
    match range.as_deref() {
        Some("bytes=2-5") => (
            StatusCode::PARTIAL_CONTENT,
            [(header::CONTENT_RANGE, "bytes 2-5/16")],
            SEGMENT[2..6].to_string(),
        )
            .into_response(),
        _ => (StatusCode::OK, SEGMENT).into_response(),
    }
}

#[tokio::test]
async fn range_survives_a_redirect_to_another_host_and_credentials_do_not() {
    // OpenList can answer a download with a 302 to the underlying storage
    //: Range must follow, Authorization must not.
    let seen = Seen::default();
    let other = serve_router(Router::new().route("/blob", get(target)).with_state(seen.clone())).await;
    let location = format!("http://{other}/blob");
    let front = serve_router(Router::new().route(
        "/f",
        get(move || {
            let location = location.clone();
            async move { (StatusCode::FOUND, [(header::LOCATION, location)]).into_response() }
        }),
    ))
    .await;

    let dav = WebDav::new(WebDavConfig {
        user: Some("user".into()),
        password: Some("secret".into()),
        ..WebDavConfig::new(format!("http://{front}/"))
    })
    .unwrap();
    let got = dav.read("f", Some(2..6)).await.unwrap();
    assert_eq!(got, Bytes::from_static(b"2345"));
    assert_eq!(dav.capabilities().range_read, Some(true));

    let seen = seen.0.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].get(header::RANGE).unwrap(), "bytes=2-5");
    // Same host, different port is a different origin: credentials must be dropped.
    assert!(seen[0].get(header::AUTHORIZATION).is_none());
}
