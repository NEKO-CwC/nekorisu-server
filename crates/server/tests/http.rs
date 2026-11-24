//! The server's HTTP surface end to end, with the real router, a synthetic
//! archive in memory, an in-memory database and a real config directory.
//! OAuth runs against a stand-in for fetching client metadata documents, so
//! nothing here touches the network.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http_body_util::BodyExt;
use nekorisu_archive::Archive;
use nekorisu_cloud::{DeviceState, FileSigner, Publisher, Signer, Store};
use nekorisu_server::auth::Auth;
use nekorisu_server::oauth::Oauth;
use nekorisu_server::oauth::client::MetadataFetcher;
use nekorisu_server::settings::Settings;
use nekorisu_server::state::AppState;
use nekorisu_server::{Shared, router};
use nekorisu_testkit::{ArchiveBuilder, FileSpec, SessionSpec, Usage, claude, codex};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tower::ServiceExt;
use url::Url;

const T: i64 = 1_794_700_800_000;
const PASSWORD: &str = "correct horse battery";
const BASE: &str = "https://nekorisu.example";
const RESOURCE: &str = "https://nekorisu.example/mcp";
const CIMD_ID: &str = "https://agent.example/oauth/client.json";
const CIMD_LIAR: &str = "https://liar.example/client.json";

struct Fixture {
    app: Router,
    state: Shared,
    setup_code: String,
    pool: SqlitePool,
    _dir: tempfile::TempDir,
}

/// Client metadata documents, served from memory.
struct Documents;

#[async_trait::async_trait]
impl MetadataFetcher for Documents {
    async fn fetch(&self, url: &Url) -> Result<Vec<u8>, String> {
        let doc = match url.as_str() {
            CIMD_ID => json!({
                "client_id": CIMD_ID, "client_name": "Example <Agent>",
                "redirect_uris": ["http://localhost/callback", "http://127.0.0.1/callback"],
                "grant_types": ["authorization_code", "refresh_token"],
                "token_endpoint_auth_method": "none",
            }),
            // Claims to be someone else.
            CIMD_LIAR => json!({
                "client_id": CIMD_ID, "client_name": "Liar",
                "redirect_uris": ["http://localhost/callback"],
            }),
            _ => return Err("HTTP 404".to_string()),
        };
        Ok(doc.to_string().into_bytes())
    }
}

fn u(i: u64, o: u64) -> Usage {
    Usage {
        input: i,
        output: o,
        cache_read: 0,
        cache_write: 0,
    }
}

fn archive() -> Archive {
    let mut b = ArchiveBuilder::new();
    b.device("dev-1", "mac-mini", T);
    let main = vec![
        claude::user(T, "first question"),
        claude::assistant_text(T + 1000, "m1", "model-a", u(10, 20), "first answer"),
        claude::tool_result(T + 1500, "tool output"),
        claude::user(T + 100_000, "second question"),
        claude::assistant_text(T + 101_000, "m2", "model-a", u(30, 40), "second answer"),
    ];
    let mut c = SessionSpec::new(
        "claude-code",
        "c1",
        "dev-1",
        T + 200_000,
        vec![FileSpec::transcript("projects/p/c1.jsonl", main)],
    );
    c.title = Some("A synthetic session".into());
    b.session(c);
    let t1 = vec![
        codex::session_meta(T + 300_000, "t1", "0.167.0", "/work/x", None),
        codex::user(T + 301_000, "codex question"),
        codex::token_count(T + 302_000, 1000, 200, 100, 0),
        codex::agent_message(T + 303_000, "codex answer"),
    ];
    b.session(SessionSpec::new(
        "codex",
        "t1",
        "dev-1",
        T + 400_000,
        vec![FileSpec::transcript("sessions/t1.jsonl", t1)],
    ));
    Archive::new(Arc::new(b.into_memory()))
}

/// `public_url` None: MCP and OAuth are off.
async fn fixture(public_url: Option<&str>) -> Fixture {
    fixture_with(public_url, &[]).await
}

/// With more environment variables.
async fn fixture_with(public_url: Option<&str>, more_env: &[(&str, &str)]) -> Fixture {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let store = Store::new(pool.clone());
    store.migrate().await.unwrap();
    let auth = Auth::new(pool.clone()).await.unwrap();
    let setup_code = auth.prepare_setup().await.unwrap().unwrap();

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("resources")).unwrap();
    std::fs::write(dir.path().join("resources/summary.sh"), "echo hi").unwrap();
    std::fs::write(
        dir.path().join("distribution.toml"),
        r#"
min_client_version = "0.1.0"
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
"#,
    )
    .unwrap();
    let signer: Arc<dyn Signer> = Arc::new(FileSigner::generate());
    let publisher = Publisher::new(dir.path().to_path_buf(), store.clone(), signer.clone());
    publisher.check().await.unwrap();

    let env = |k: &str| -> Option<String> {
        match k {
            "NEKORISU_DEVICE_TOKEN" => Some("device-token".to_string()),
            "NEKORISU_PUBLIC_URL" => public_url.map(str::to_owned),
            _ => more_env.iter().find(|(name, _)| *name == k).map(|(_, v)| v.to_string()),
        }
    };
    let settings = Settings::from_env(&env).unwrap();
    let oauth = Oauth::new(pool.clone(), &settings, Arc::new(Documents)).await.unwrap();
    let state: Shared = Arc::new(AppState {
        settings,
        archive: Arc::new(archive()),
        store,
        auth,
        oauth,
        signer,
        source: publisher.status(),
    });
    Fixture {
        app: router(state.clone()),
        state,
        setup_code,
        pool,
        _dir: dir,
    }
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes().to_vec();
    (parts.status, parts.headers, bytes)
}

fn get(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut r = Request::builder().uri(path);
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, c);
    }
    r.body(Body::empty()).unwrap()
}

fn post_json(path: &str, body: Value, cookie: Option<&str>) -> Request<Body> {
    let mut r = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, c);
    }
    r.body(Body::from(body.to_string())).unwrap()
}

fn json_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(bytes)))
}

/// Sets the password and returns the session cookie.
async fn sign_in(f: &Fixture) -> String {
    let (status, headers, _) = send(
        &f.app,
        post_json(
            "/api/auth/setup",
            json!({ "password": PASSWORD, "setup_code": f.setup_code }),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let cookie = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
    cookie.split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn the_panel_needs_a_login_and_says_so() {
    let f = fixture(Some(BASE)).await;
    let (status, headers, body) = send(&f.app, get("/api/overview", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(&body)["error"]["code"], "unauthenticated");
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
    assert!(
        headers
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    assert_eq!(headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");

    let (status, _, body) = send(&f.app, get("/api/auth/state", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&body), json!({ "initialized": false, "authenticated": false }));

    let (status, _, body) = send(&f.app, get("/healthz", None)).await;
    assert_eq!((status, body.as_slice()), (StatusCode::OK, b"ok".as_slice()));
    // An unknown /api path is a JSON 404, not the app.
    let (status, _, body) = send(&f.app, get("/api/nope", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json_of(&body)["error"]["code"], "not_found");
}

#[tokio::test]
async fn first_run_setup_login_and_logout() {
    let f = fixture(Some(BASE)).await;
    let wrong = post_json(
        "/api/auth/setup",
        json!({ "password": PASSWORD, "setup_code": "WRONG-CODE" }),
        None,
    );
    let (status, _, body) = send(&f.app, wrong).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json_of(&body)["error"]["code"], "bad_setup_code");

    let weak = post_json(
        "/api/auth/setup",
        json!({ "password": "short", "setup_code": f.setup_code }),
        None,
    );
    let (status, _, body) = send(&f.app, weak).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json_of(&body)["error"]["code"], "weak_password");

    let cookie = sign_in(&f).await;
    let (_, _, body) = send(&f.app, get("/api/auth/state", Some(&cookie))).await;
    assert_eq!(json_of(&body), json!({ "initialized": true, "authenticated": true }));
    assert_eq!(
        send(&f.app, get("/api/overview", Some(&cookie))).await.0,
        StatusCode::OK
    );

    // Setup works once.
    let again = post_json(
        "/api/auth/setup",
        json!({ "password": PASSWORD, "setup_code": f.setup_code }),
        None,
    );
    let (status, _, body) = send(&f.app, again).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json_of(&body)["error"]["code"], "already_initialized");

    // The cookie is HttpOnly and SameSite=Strict.
    let login = post_json("/api/auth/login", json!({ "password": PASSWORD }), None);
    let (status, headers, _) = send(&f.app, login).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let set = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict"));

    let bad = post_json("/api/auth/login", json!({ "password": "nope nope nope" }), None);
    let (status, _, body) = send(&f.app, bad).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(&body)["error"]["code"], "bad_credentials");

    let logout = Request::builder()
        .method("POST")
        .uri("/api/auth/logout")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send(&f.app, logout).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        headers
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(
        send(&f.app, get("/api/overview", Some(&cookie))).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn overview_sessions_and_reading() {
    let f = fixture(Some(BASE)).await;
    let c = sign_in(&f).await;

    let (_, _, body) = send(&f.app, get("/api/overview", Some(&c))).await;
    let o = json_of(&body);
    assert_eq!(o["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(o["source_url"], "https://github.com/NEKO-CwC/nekorisu-server");
    assert_eq!(o["storage"]["ok"], true);
    assert_eq!(o["counts"]["sessions"], 2);
    assert_eq!(o["counts"]["by_agent"]["codex"], 1);
    assert_eq!(o["config"]["serial"], 2);
    assert!(o["last_upload_ms"].is_i64());

    let (_, _, body) = send(&f.app, get("/api/sessions?agent=claude-code", Some(&c))).await;
    let list = json_of(&body);
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["ref"], "claude-code:c1");
    assert_eq!(list["items"][0]["title"], "A synthetic session");
    assert_eq!(list["items"][0]["devices"][0]["name"], "mac-mini");
    let (status, _, _) = send(&f.app, get("/api/sessions?limit=abc", Some(&c))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, body) = send(&f.app, get("/api/sessions/claude-code/c1", Some(&c))).await;
    assert_eq!(status, StatusCode::OK);
    let d = json_of(&body);
    assert_eq!(d["selected_version"], "dev-1-1");
    assert_eq!(d["files"][0]["lines"], 5);
    let (status, _, body) = send(&f.app, get("/api/sessions/claude-code/nope", Some(&c))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json_of(&body)["error"]["code"], "session_not_found");

    // The reader: only what the person said, then replies, with text and no raw by default.
    let (_, _, body) = send(
        &f.app,
        get("/api/sessions/claude-code/c1/lines?kinds=user,assistant", Some(&c)),
    )
    .await;
    let l = json_of(&body);
    assert_eq!(l["total_lines"], 5);
    assert_eq!(l["matched"], 4, "the tool result is not a user or assistant line");
    assert_eq!(l["items"][0]["text"], "first question");
    assert_eq!(l["items"][0]["kind"], "user");
    assert_eq!(l["items"][1]["text"], "first answer");
    assert!(l["items"][0]["raw"].is_null());
    assert_eq!(l["items"][0]["ts_ms"], T);

    let (_, _, body) = send(
        &f.app,
        get("/api/sessions/claude-code/c1/lines?from=3&to=3&raw=1", Some(&c)),
    )
    .await;
    let l = json_of(&body);
    assert_eq!(l["items"][0]["text"], "tool result: tool output");
    assert!(l["items"][0]["raw"].as_str().unwrap().contains("tool_result"));

    let (_, _, body) = send(
        &f.app,
        get(
            "/api/sessions/claude-code/c1/lines?kinds=user&max_field_chars=5",
            Some(&c),
        ),
    )
    .await;
    let l = json_of(&body);
    assert_eq!(l["items"][0]["text"], "first");
    assert_eq!(l["items"][0]["text_truncated"], true);

    let (status, _, _) = send(
        &f.app,
        get("/api/sessions/claude-code/c1/lines?kinds=nonsense", Some(&c)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn devices_config_storage_and_activity() {
    let f = fixture(Some(BASE)).await;
    let c = sign_in(&f).await;

    // dev-1 reports; "ghost" reports but never uploaded.
    for (id, serial) in [("dev-1", 1u64), ("ghost", 2)] {
        f.state
            .store
            .upsert_report(
                id,
                &DeviceState {
                    received_ms: T,
                    client_version: "0.1.0".into(),
                    applied_serial: serial,
                    requests: vec![nekorisu_cloud::RequestState {
                        id: "f-1".into(),
                        state: "pending".into(),
                    }],
                    local_values: vec![nekorisu_cloud::LocalValue {
                        key: "local.copy_form".into(),
                        value_json: "\"pruned\"".into(),
                        locked: true,
                    }],
                    ..DeviceState::default()
                },
            )
            .await
            .unwrap();
    }
    let (_, _, body) = send(&f.app, get("/api/devices", Some(&c))).await;
    let devices = json_of(&body)["items"].as_array().unwrap().clone();
    assert_eq!(devices.len(), 2);
    let dev1 = devices.iter().find(|d| d["id"] == "dev-1").unwrap();
    assert_eq!(dev1["name"], "mac-mini");
    assert_eq!(dev1["sessions"], 2);
    assert_eq!(dev1["state"]["behind"], 1, "serial 2 is current, the device applied 1");
    assert_eq!(dev1["state"]["verified"], false);
    assert_eq!(dev1["state"]["local_values"][0]["locked"], true);
    assert_eq!(dev1["state"]["requests"][0]["state"], "pending");
    let ghost = devices.iter().find(|d| d["id"] == "ghost").unwrap();
    assert!(ghost["name"].is_null());
    assert_eq!(ghost["state"]["behind"], 0);

    let (_, _, body) = send(&f.app, get("/api/config", Some(&c))).await;
    let cfg = json_of(&body);
    assert_eq!(cfg["current"]["serial"], 2);
    assert_eq!(cfg["current"]["payload"]["serial"], 2);
    assert_eq!(cfg["current"]["resources"][0]["executable"], true);
    assert_eq!(cfg["public"], json!({ "enabled": false, "serial": 1 }));
    assert_eq!(cfg["device_token_set"], true);
    let command = cfg["signing"]["connect_command"].as_str().unwrap();
    assert!(command.starts_with("nekorisu source add home https://nekorisu.example --key "));
    assert!(command.ends_with(cfg["signing"]["public_key"].as_str().unwrap()));
    assert_eq!(cfg["history"].as_array().unwrap().len(), 2);
    let force = &cfg["requests"][0];
    assert_eq!(
        (force["kind"].as_str(), force["id"].as_str()),
        (Some("force"), Some("f-1"))
    );
    assert_eq!(force["changes"][0]["key"], "local.copy_form");
    assert_eq!(
        force["devices"][0],
        json!({ "id": "dev-1", "name": "mac-mini", "state": "pending" })
    );
    assert!(cfg["source"]["last_error"].is_null());

    let (_, _, body) = send(&f.app, get("/api/storage", Some(&c))).await;
    let st = json_of(&body);
    assert_eq!(st["ok"], true);
    assert_eq!(st["layout"]["found"], true);
    assert!(
        st["layout"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "sessions")
    );

    let (_, _, body) = send(&f.app, get("/api/activity?limit=10", Some(&c))).await;
    let a = json_of(&body);
    assert_eq!(a["items"].as_array().unwrap().len(), 2);
    assert_eq!(a["items"][0]["op"], "archive");
    assert_eq!(a["items"][0]["device"]["name"], "mac-mini");
    assert_eq!(a["skipped_lines"], 0);

    let (status, _, body) = send(&f.app, post_json("/api/refresh", json!({}), Some(&c))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&body)["sessions"], 2);
}

#[tokio::test]
async fn export_downloads_jsonl_and_csv() {
    let f = fixture(Some(BASE)).await;
    let c = sign_in(&f).await;

    let (_, _, body) = send(&f.app, get("/api/export/fields", Some(&c))).await;
    let fields = json_of(&body);
    assert_eq!(fields["formats"], json!(["jsonl", "csv"]));
    assert!(
        fields["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["name"] == "prev_reply")
    );

    let (status, headers, body) = send(&f.app, get("/api/export?fields=session,turn,user,tokens", Some(&c))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("application/x-ndjson")
    );
    let disposition = headers.get(header::CONTENT_DISPOSITION).unwrap().to_str().unwrap();
    assert!(disposition.starts_with("attachment; filename=\"nekorisu-export-") && disposition.ends_with(".jsonl\""));
    let rows: Vec<Value> = String::from_utf8(body).unwrap().lines().map(json_of_str).collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["user"], "first question");
    assert_eq!(rows[0]["tokens"]["output"], 20);
    assert_eq!(rows[1]["turn"], 2);
    assert_eq!(rows[2]["session"], "codex:t1");

    let (_, headers, body) = send(
        &f.app,
        get("/api/export?format=csv&fields=turn,user&session=codex:t1", Some(&c)),
    )
    .await;
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/csv")
    );
    assert_eq!(String::from_utf8(body).unwrap(), "turn,user\r\n1,codex question\r\n");

    let (status, _, body) = send(&f.app, get("/api/export?fields=nope", Some(&c))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json_of(&body)["error"]["code"], "bad_request");
    assert_eq!(
        send(&f.app, get("/api/export?format=xml", Some(&c))).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(send(&f.app, get("/api/export", None)).await.0, StatusCode::UNAUTHORIZED);
}

fn json_of_str(line: &str) -> Value {
    json_of(line.as_bytes())
}

fn mcp(token: Option<&str>, body: Value) -> Request<Body> {
    let mut r = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        r = r.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    r.body(Body::from(body.to_string())).unwrap()
}

async fn tool(app: &Router, token: &str, name: &str, args: Value) -> (bool, String) {
    let request =
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } });
    let (status, _, body) = send(app, mcp(Some(token), request)).await;
    assert_eq!(status, StatusCode::OK);
    let reply = json_of(&body);
    let result = &reply["result"];
    (
        result["isError"] == true,
        result["content"][0]["text"].as_str().unwrap().to_string(),
    )
}

// ---- OAuth helpers ----

const VERIFIER: &str = "a-synthetic-pkce-verifier-long-enough-for-rfc-7636";
const REDIRECT: &str = "http://localhost:51234/callback";

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()).as_slice())
}

fn form(path: &str, pairs: &[(&str, &str)]) -> Request<Body> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
}

/// `/oauth/authorize?…` with everything right, then `changes` (None removes a parameter).
fn authorize_path(client_id: &str, redirect: &str, changes: &[(&str, Option<&str>)]) -> String {
    let mut pairs: Vec<(String, String)> = [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect),
        ("code_challenge", &challenge(VERIFIER)),
        ("code_challenge_method", "S256"),
        ("state", "state-1"),
        ("resource", RESOURCE),
        ("scope", "nekorisu:read"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for (key, value) in changes {
        pairs.retain(|(k, _)| k != key);
        if let Some(v) = value {
            pairs.push((key.to_string(), v.to_string()));
        }
    }
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(&pairs)
        .finish();
    format!("/oauth/authorize?{query}")
}

/// A hidden field's value on the consent page.
fn hidden(html: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\" value=\"");
    let start = html.find(&marker).unwrap_or_else(|| panic!("no {name} in {html}")) + marker.len();
    html[start..].split('"').next().unwrap().to_string()
}

fn redirect_query(headers: &HeaderMap) -> (String, HashMap<String, String>) {
    let location = headers.get(header::LOCATION).unwrap().to_str().unwrap();
    let url = Url::parse(location).unwrap();
    let query = url.query_pairs().into_owned().collect();
    (location.to_string(), query)
}

async fn register(f: &Fixture, redirect: &str) -> String {
    let body = json!({
        "client_name": "Test <Agent>", "redirect_uris": [redirect],
        "token_endpoint_auth_method": "none",
        "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"],
    });
    let (status, headers, body) = send(&f.app, post_json("/oauth/register", body, None)).await;
    assert_eq!(status, StatusCode::CREATED, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
    let reply = json_of(&body);
    assert_eq!(reply["token_endpoint_auth_method"], "none");
    assert_eq!(reply["redirect_uris"], json!([redirect]));
    reply["client_id"].as_str().unwrap().to_string()
}

/// Opens the consent page; returns its HTML.
async fn consent_page(f: &Fixture, path: &str) -> String {
    let (status, _, body) = send(&f.app, get(path, None)).await;
    let html = String::from_utf8(body).unwrap();
    assert_eq!(status, StatusCode::OK, "{html}");
    html
}

async fn answer(f: &Fixture, html: &str, password: &str, action: &str) -> (StatusCode, HeaderMap, String) {
    let request = form(
        "/oauth/authorize",
        &[
            ("request_id", &hidden(html, "request_id")),
            ("csrf", &hidden(html, "csrf")),
            ("password", password),
            ("action", action),
        ],
    );
    let (status, headers, body) = send(&f.app, request).await;
    (status, headers, String::from_utf8(body).unwrap())
}

/// The whole consent step; returns the code.
async fn code_for(f: &Fixture, client_id: &str, redirect: &str) -> String {
    let html = consent_page(f, &authorize_path(client_id, redirect, &[])).await;
    let (status, headers, _) = answer(f, &html, PASSWORD, "approve").await;
    assert_eq!(status, StatusCode::FOUND);
    redirect_query(&headers).1["code"].clone()
}

async fn token_request(f: &Fixture, pairs: &[(&str, &str)]) -> (StatusCode, Value) {
    let (status, headers, body) = send(&f.app, form("/oauth/token", pairs)).await;
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
    (status, json_of(&body))
}

async fn exchange(f: &Fixture, client_id: &str, code: &str, verifier: &str) -> (StatusCode, Value) {
    token_request(
        f,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", REDIRECT),
            ("resource", RESOURCE),
        ],
    )
    .await
}

async fn refresh(f: &Fixture, client_id: &str, token: &str) -> (StatusCode, Value) {
    token_request(
        f,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", token),
            ("resource", RESOURCE),
        ],
    )
    .await
}

/// Registers, signs in and returns (client id, token response).
async fn connect(f: &Fixture) -> (String, Value) {
    let client_id = register(f, "http://localhost/callback").await;
    let code = code_for(f, &client_id, REDIRECT).await;
    let (status, tokens) = exchange(f, &client_id, &code, VERIFIER).await;
    assert_eq!(status, StatusCode::OK, "{tokens}");
    (client_id, tokens)
}

fn access(tokens: &Value) -> &str {
    tokens["access_token"].as_str().unwrap()
}

async fn mcp_status(f: &Fixture, token: &str) -> StatusCode {
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    send(&f.app, mcp(Some(token), ping)).await.0
}

// ---- MCP and OAuth ----

#[tokio::test]
async fn mcp_and_oauth_are_off_without_a_public_url() {
    let f = fixture(None).await;
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    assert_eq!(send(&f.app, mcp(Some("anything"), ping)).await.0, StatusCode::NOT_FOUND);
    for path in [
        "/.well-known/oauth-protected-resource/mcp",
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-authorization-server",
        "/oauth/authorize?client_id=x",
    ] {
        assert_eq!(send(&f.app, get(path, None)).await.0, StatusCode::NOT_FOUND, "{path}");
    }
    let register = post_json("/oauth/register", json!({ "redirect_uris": [REDIRECT] }), None);
    assert_eq!(send(&f.app, register).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        send(&f.app, form("/oauth/token", &[("grant_type", "refresh_token")]))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn discovery_documents_and_the_challenge() {
    let f = fixture(Some(BASE)).await;
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let (status, headers, _) = send(&f.app, mcp(None, ping.clone())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers.get(header::WWW_AUTHENTICATE).unwrap(),
        "Bearer resource_metadata=\"https://nekorisu.example/.well-known/oauth-protected-resource/mcp\", scope=\"nekorisu:read\""
    );
    let (status, headers, _) = send(&f.app, mcp(Some("nka_made-up"), ping)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        headers
            .get(header::WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with(", error=\"invalid_token\"")
    );

    let expected = json!({
        "resource": RESOURCE,
        "authorization_servers": [BASE],
        "scopes_supported": ["nekorisu:read"],
        "bearer_methods_supported": ["header"],
        "resource_name": "nekorisu-server",
    });
    for path in [
        "/.well-known/oauth-protected-resource/mcp",
        "/.well-known/oauth-protected-resource",
    ] {
        let (status, _, body) = send(&f.app, get(path, None)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_of(&body), expected, "{path}");
    }

    let (status, _, body) = send(&f.app, get("/.well-known/oauth-authorization-server", None)).await;
    assert_eq!(status, StatusCode::OK);
    let m = json_of(&body);
    assert_eq!(m["issuer"], BASE);
    assert_eq!(m["authorization_endpoint"], "https://nekorisu.example/oauth/authorize");
    assert_eq!(m["token_endpoint"], "https://nekorisu.example/oauth/token");
    assert_eq!(m["registration_endpoint"], "https://nekorisu.example/oauth/register");
    assert_eq!(m["response_types_supported"], json!(["code"]));
    assert_eq!(
        m["grant_types_supported"],
        json!(["authorization_code", "refresh_token"])
    );
    assert_eq!(m["code_challenge_methods_supported"], json!(["S256"]));
    assert_eq!(m["token_endpoint_auth_methods_supported"], json!(["none"]));
    assert_eq!(m["scopes_supported"], json!(["nekorisu:read"]));
    assert_eq!(m["client_id_metadata_document_supported"], true);
    assert_eq!(m["authorization_response_iss_parameter_supported"], true);
    // A discovery path the server does not have is a 404, not the panel.
    assert_eq!(
        send(&f.app, get("/.well-known/openid-configuration", None)).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn dynamic_registration_checks_what_it_is_given() {
    let f = fixture(Some(BASE)).await;
    let id = register(&f, REDIRECT).await;
    assert!(id.starts_with("nk-"));
    assert_ne!(register(&f, REDIRECT).await, id, "every registration is a new client");

    let refused = [
        (json!({ "client_name": "x" }), "invalid_redirect_uri"),
        (json!({ "redirect_uris": [] }), "invalid_redirect_uri"),
        (
            json!({ "redirect_uris": ["http://agent.example/cb"] }),
            "invalid_redirect_uri",
        ),
        (json!({ "redirect_uris": ["myagent://cb"] }), "invalid_redirect_uri"),
        (
            json!({ "redirect_uris": ["https://agent.example/cb#x"] }),
            "invalid_redirect_uri",
        ),
        (json!({ "redirect_uris": [REDIRECT, 7] }), "invalid_redirect_uri"),
        (
            json!({ "redirect_uris": [REDIRECT], "grant_types": ["client_credentials"] }),
            "invalid_client_metadata",
        ),
        (
            json!({ "redirect_uris": [REDIRECT], "response_types": ["token"] }),
            "invalid_client_metadata",
        ),
        (json!(["not", "an", "object"]), "invalid_client_metadata"),
    ];
    for (body, code) in refused {
        let (status, _, reply) = send(&f.app, post_json("/oauth/register", body.clone(), None)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(json_of(&reply)["error"], code, "{body}");
    }
}

#[tokio::test]
async fn sign_in_end_to_end_then_tools() {
    let f = fixture(Some(BASE)).await;
    sign_in(&f).await;
    // Registered without a port; the agent comes back on whatever port it took.
    let client_id = register(&f, "http://localhost/callback").await;

    let path = authorize_path(&client_id, REDIRECT, &[]);
    let (status, headers, body) = send(&f.app, get(&path, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get(header::X_FRAME_OPTIONS).unwrap(), "DENY");
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
    let csp = headers.get(header::CONTENT_SECURITY_POLICY).unwrap().to_str().unwrap();
    assert!(
        csp.contains("frame-ancestors 'none'") && csp.contains("default-src 'none'"),
        "{csp}"
    );
    assert!(csp.contains("form-action 'self' http://localhost:51234"), "{csp}");
    let html = String::from_utf8(body).unwrap();
    assert!(
        html.contains("Test &lt;Agent&gt;") && !html.contains("Test <Agent>"),
        "escaped name"
    );
    assert!(html.contains("这台电脑上的一个程序") && html.contains("localhost:51234"));
    assert!(html.contains("只读") && html.contains("type=\"password\""));

    // A wrong password shows the page again; the same request can still be answered.
    let (status, headers, page) = answer(&f, &html, "not the password", "approve").await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::LOCATION).is_none());
    assert!(page.contains("密码不对"));
    // A forged CSRF token is refused.
    let forged = form(
        "/oauth/authorize",
        &[
            ("request_id", &hidden(&html, "request_id")),
            ("csrf", "0000"),
            ("password", PASSWORD),
            ("action", "approve"),
        ],
    );
    let (status, headers, _) = send(&f.app, forged).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(headers.get(header::LOCATION).is_none());

    let (status, headers, _) = answer(&f, &html, PASSWORD, "approve").await;
    assert_eq!(status, StatusCode::FOUND);
    let (location, query) = redirect_query(&headers);
    assert!(location.starts_with("http://localhost:51234/callback?"), "{location}");
    assert_eq!(query["state"], "state-1");
    assert_eq!(query["iss"], BASE);
    let code = query["code"].clone();
    // The consent request is single-use.
    let (status, _, _) = answer(&f, &html, PASSWORD, "approve").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, tokens) = exchange(&f, &client_id, &code, VERIFIER).await;
    assert_eq!(status, StatusCode::OK, "{tokens}");
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["expires_in"], 3600);
    assert_eq!(tokens["scope"], "nekorisu:read");
    let token = access(&tokens).to_string();
    assert!(token.starts_with("nka_") && tokens["refresh_token"].as_str().unwrap().starts_with("nkr_"));

    let init =
        json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } });
    let (_, _, body) = send(&f.app, mcp(Some(&token), init)).await;
    let reply = json_of(&body);
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(reply["result"]["serverInfo"]["name"], "nekorisu-server");

    let (status, _, body) = send(
        &f.app,
        mcp(
            Some(&token),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<String> = json_of(&body)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        ["list_sessions", "get_session", "read_lines", "list_devices", "export"]
    );

    // A notification gets no reply body; an unknown method an error.
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    assert_eq!(send(&f.app, mcp(Some(&token), note)).await.0, StatusCode::ACCEPTED);
    let (_, _, body) = send(
        &f.app,
        mcp(Some(&token), json!({ "jsonrpc": "2.0", "id": 4, "method": "nope" })),
    )
    .await;
    assert_eq!(json_of(&body)["error"]["code"], -32601);
}

#[tokio::test]
async fn pkce_redirect_and_resource_are_checked() {
    let f = fixture(Some(BASE)).await;
    sign_in(&f).await;
    let client_id = register(&f, "http://localhost/callback").await;

    // Problems with the client or its redirect URI are shown, never redirected.
    for path in [
        authorize_path(&client_id, "http://localhost:51234/elsewhere", &[]),
        authorize_path(&client_id, "http://127.0.0.1:51234/callback", &[]),
        authorize_path(&client_id, "https://evil.example/callback", &[]),
        authorize_path(&client_id, REDIRECT, &[("redirect_uri", None)]),
        authorize_path("nk-unregistered", REDIRECT, &[]),
        authorize_path(&client_id, REDIRECT, &[("client_id", None)]),
    ] {
        let (status, headers, body) = send(&f.app, get(&path, None)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert!(headers.get(header::LOCATION).is_none(), "{path}");
        assert!(String::from_utf8(body).unwrap().contains("无法授权"));
    }

    // Once the redirect URI is known to be the client's, errors go back to it with state and iss.
    for (changes, error) in [
        (vec![("resource", Some("https://other.example/mcp"))], "invalid_target"),
        (vec![("code_challenge", None)], "invalid_request"),
        (vec![("code_challenge_method", Some("plain"))], "invalid_request"),
        (vec![("code_challenge_method", None)], "invalid_request"),
        (vec![("response_type", Some("token"))], "unsupported_response_type"),
        (vec![("scope", Some("nekorisu:write"))], "invalid_scope"),
    ] {
        let path = authorize_path(&client_id, REDIRECT, &changes);
        let (status, headers, _) = send(&f.app, get(&path, None)).await;
        assert_eq!(status, StatusCode::FOUND, "{path}");
        let (location, query) = redirect_query(&headers);
        assert!(location.starts_with(REDIRECT), "{location}");
        assert_eq!(query["error"], error, "{path}");
        assert_eq!(query["state"], "state-1");
        assert_eq!(query["iss"], BASE);
    }
    // No resource parameter means this server's resource.
    consent_page(&f, &authorize_path(&client_id, REDIRECT, &[("resource", None)])).await;

    let code = code_for(&f, &client_id, REDIRECT).await;
    let (status, reply) = exchange(
        &f,
        &client_id,
        &code,
        "a-wrong-verifier-that-is-also-long-enough-for-pkce",
    )
    .await;
    assert_eq!(
        (status, reply["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_grant"))
    );
    let (status, reply) = token_request(
        &f,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &client_id),
            ("code", &code),
            ("code_verifier", VERIFIER),
            ("redirect_uri", "http://localhost:9/callback"),
        ],
    )
    .await;
    assert_eq!(
        (status, reply["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_grant"))
    );
    let (status, reply) = token_request(
        &f,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &client_id),
            ("code", &code),
            ("code_verifier", VERIFIER),
            ("redirect_uri", REDIRECT),
            ("resource", "https://other.example/mcp"),
        ],
    )
    .await;
    assert_eq!(
        (status, reply["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_target"))
    );
    let (status, reply) = exchange(&f, "nk-someone-else", &code, VERIFIER).await;
    assert_eq!(
        (status, reply["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_grant"))
    );

    // Failed attempts do not use the code up; the right one works…
    let (status, tokens) = exchange(&f, &client_id, &code, VERIFIER).await;
    assert_eq!(status, StatusCode::OK, "{tokens}");
    assert_eq!(mcp_status(&f, access(&tokens)).await, StatusCode::OK);
    // …once. A second use revokes what the first one got.
    let (status, reply) = exchange(&f, &client_id, &code, VERIFIER).await;
    assert_eq!(
        (status, reply["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_grant"))
    );
    assert_eq!(mcp_status(&f, access(&tokens)).await, StatusCode::UNAUTHORIZED);
    let (status, _) = refresh(&f, &client_id, tokens["refresh_token"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, reply) = token_request(&f, &[("grant_type", "password"), ("client_id", &client_id)]).await;
    assert_eq!(
        (status, reply["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("unsupported_grant_type"))
    );
    let (status, reply) = token_request(&f, &[("grant_type", "refresh_token"), ("grant_type", "refresh_token")]).await;
    assert_eq!(
        (status, reply["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_request"))
    );
}

#[tokio::test]
async fn refresh_tokens_rotate_and_reuse_revokes_the_grant() {
    let f = fixture(Some(BASE)).await;
    sign_in(&f).await;
    let (client_id, first) = connect(&f).await;
    let old_refresh = first["refresh_token"].as_str().unwrap();

    let (status, second) = refresh(&f, &client_id, old_refresh).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["scope"], "nekorisu:read");
    assert_ne!(second["access_token"], first["access_token"]);
    assert_ne!(second["refresh_token"], first["refresh_token"]);
    assert_eq!(mcp_status(&f, access(&second)).await, StatusCode::OK);

    // Another client cannot use it.
    let (status, reply) = refresh(&f, "nk-someone-else", second["refresh_token"].as_str().unwrap()).await;
    assert_eq!(
        (status, reply["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_grant"))
    );

    // The retired refresh token comes back: the whole grant is revoked.
    let (status, reply) = refresh(&f, &client_id, old_refresh).await;
    assert_eq!(
        (status, reply["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_grant"))
    );
    assert_eq!(mcp_status(&f, access(&second)).await, StatusCode::UNAUTHORIZED);
    let (status, _) = refresh(&f, &client_id, second["refresh_token"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn expired_and_foreign_tokens_are_refused() {
    let f = fixture(Some(BASE)).await;
    sign_in(&f).await;
    let (client_id, tokens) = connect(&f).await;
    assert_eq!(mcp_status(&f, access(&tokens)).await, StatusCode::OK);

    sqlx::query("UPDATE oauth_tokens SET expires_ms = 1 WHERE kind = 'access'")
        .execute(&f.pool)
        .await
        .unwrap();
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let (status, headers, _) = send(&f.app, mcp(Some(access(&tokens)), ping)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        headers
            .get(header::WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("error=\"invalid_token\"")
    );
    // The agent refreshes and goes on.
    let (status, fresh) = refresh(&f, &client_id, tokens["refresh_token"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(mcp_status(&f, access(&fresh)).await, StatusCode::OK);

    // A token for another resource (say the public address changed) is not accepted.
    sqlx::query("UPDATE oauth_grants SET resource = 'https://old.example/mcp'")
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(mcp_status(&f, access(&fresh)).await, StatusCode::UNAUTHORIZED);
    // Nor is the refresh token's grant any good for this resource.
    let (status, reply) = refresh(&f, &client_id, fresh["refresh_token"].as_str().unwrap()).await;
    assert_eq!(
        (status, reply["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_target"))
    );
}

#[tokio::test]
async fn apps_are_listed_and_revoked_in_the_panel() {
    let f = fixture(Some(BASE)).await;
    let cookie = sign_in(&f).await;
    let (_, tokens) = connect(&f).await;

    assert_eq!(
        send(&f.app, get("/api/oauth/apps", None)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, _, body) = send(&f.app, get("/api/oauth/apps", Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);
    let list = json_of(&body);
    assert_eq!(list["mcp_url"], RESOURCE);
    let apps = list["items"].as_array().unwrap();
    assert_eq!(apps.len(), 1);
    let app = &apps[0];
    assert_eq!(app["client_name"], "Test <Agent>");
    assert_eq!(app["redirect_host"], "localhost:51234");
    assert_eq!(app["loopback"], true);
    assert_eq!(app["metadata_document"], false);
    assert_eq!(app["scope"], "nekorisu:read");
    assert!(app["created_ms"].is_i64() && app["last_used_ms"].is_i64());
    // Nothing secret is listed.
    assert!(!body.windows(4).any(|w| w == b"nka_" || w == b"nkr_"));

    let id = app["id"].as_str().unwrap();
    let revoke = format!("/api/oauth/apps/{id}/revoke");
    assert_eq!(
        send(&f.app, post_json(&revoke, json!({}), None)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&f.app, post_json(&revoke, json!({}), Some(&cookie))).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(mcp_status(&f, access(&tokens)).await, StatusCode::UNAUTHORIZED);
    let (_, _, body) = send(&f.app, get("/api/oauth/apps", Some(&cookie))).await;
    assert_eq!(json_of(&body)["items"], json!([]));
    let (status, _, body) = send(&f.app, post_json(&revoke, json!({}), Some(&cookie))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json_of(&body)["error"]["code"], "app_not_found");
}

#[tokio::test]
async fn client_metadata_documents() {
    let f = fixture(Some(BASE)).await;
    sign_in(&f).await;

    let html = consent_page(&f, &authorize_path(CIMD_ID, "http://localhost:40000/callback", &[])).await;
    assert!(
        html.contains("Example &lt;Agent&gt;") && html.contains("agent.example"),
        "{html}"
    );
    let (status, headers, _) = answer(&f, &html, PASSWORD, "approve").await;
    assert_eq!(status, StatusCode::FOUND);
    let code = redirect_query(&headers).1["code"].clone();
    let (status, tokens) = token_request(
        &f,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", CIMD_ID),
            ("code", &code),
            ("code_verifier", VERIFIER),
            ("redirect_uri", "http://localhost:40000/callback"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tokens}");
    assert_eq!(mcp_status(&f, access(&tokens)).await, StatusCode::OK);

    for (client_id, redirect) in [
        // Its document names another client_id.
        (CIMD_LIAR, "http://localhost:40000/callback"),
        // Not a redirect URI its document lists.
        (CIMD_ID, "https://agent.example/callback"),
        // No document there (the fetcher fails).
        ("https://nowhere.example/client.json", "http://localhost:40000/callback"),
        // Not written the canonical way, or not https.
        (
            "https://agent.example/oauth/../oauth/client.json",
            "http://localhost:40000/callback",
        ),
        (
            "http://agent.example/oauth/client.json",
            "http://localhost:40000/callback",
        ),
    ] {
        let path = authorize_path(client_id, redirect, &[]);
        let (status, headers, _) = send(&f.app, get(&path, None)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{client_id} {redirect}");
        assert!(headers.get(header::LOCATION).is_none());
    }
}

#[tokio::test]
async fn deny_no_password_yet_and_the_shared_limiter() {
    let f = fixture(Some(BASE)).await;
    let client_id = register(&f, "http://localhost/callback").await;
    let path = authorize_path(&client_id, REDIRECT, &[]);

    // No administrator password yet: nobody can approve.
    let (status, _, body) = send(&f.app, get(&path, None)).await;
    let html = String::from_utf8(body).unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("还没有设置管理员密码") && !html.contains("<form"),
        "{html}"
    );

    let cookie = sign_in(&f).await;
    let html = consent_page(&f, &path).await;
    let (status, headers, _) = answer(&f, &html, "", "deny").await;
    assert_eq!(status, StatusCode::FOUND);
    let (_, query) = redirect_query(&headers);
    assert_eq!(query["error"], "access_denied");
    assert_eq!(query["state"], "state-1");
    assert_eq!(query["iss"], BASE);
    assert!(!query.contains_key("code"));

    // Wrong passwords on the consent page count with the panel login.
    let html = consent_page(&f, &path).await;
    for _ in 0..5 {
        let (status, _, page) = answer(&f, &html, "not the password", "approve").await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("密码不对"));
    }
    let (status, _, page) = answer(&f, &html, PASSWORD, "approve").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("尝试次数太多"), "{page}");
    let login = post_json("/api/auth/login", json!({ "password": PASSWORD }), None);
    assert_eq!(send(&f.app, login).await.0, StatusCode::TOO_MANY_REQUESTS);
    // A signed-in panel still works.
    assert_eq!(
        send(&f.app, get("/api/overview", Some(&cookie))).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn mcp_tools_read_sessions_lines_and_exports() {
    let f = fixture(Some(BASE)).await;
    sign_in(&f).await;
    let (_, tokens) = connect(&f).await;
    let t = access(&tokens);

    let (error, text) = tool(&f.app, t, "list_sessions", json!({ "query": "synthetic" })).await;
    assert!(!error);
    assert!(text.starts_with("1 of 1 sessions\n"), "{text}");
    assert!(text.contains("claude-code:c1") && text.contains("mac-mini"));

    // A bare id works when it is unique.
    let (error, text) = tool(&f.app, t, "get_session", json!({ "session": "c1" })).await;
    assert!(!error, "{text}");
    let session = json_of(text.as_bytes());
    assert_eq!(session["session"], "claude-code:c1");
    assert_eq!(session["files"][0]["lines"], 5);

    let (error, text) = tool(
        &f.app,
        t,
        "read_lines",
        json!({ "session": "claude-code:c1", "kinds": ["user"] }),
    )
    .await;
    assert!(!error);
    assert!(
        text.contains("L1 user first question\n") && text.contains("L4 user second question\n"),
        "{text}"
    );
    assert!(text.starts_with("2 of 2 matching lines (5 in the file)"), "{text}");

    let (error, text) = tool(&f.app, t, "list_devices", json!({})).await;
    assert!(!error && text.contains("mac-mini"), "{text}");

    let (error, text) = tool(
        &f.app,
        t,
        "export",
        json!({ "fields": ["turn", "user"], "agent": "claude-code" }),
    )
    .await;
    assert!(!error, "{text}");
    assert_eq!(text.lines().count(), 2);
    assert!(text.lines().next().unwrap().contains("first question"));

    // Mistakes are tool errors the agent can read, not protocol errors.
    let (error, text) = tool(&f.app, t, "read_lines", json!({ "session": "nonexistent" })).await;
    assert!(error && text.contains("no session"), "{text}");
    let (error, _) = tool(&f.app, t, "read_lines", json!({ "session": "c1", "kinds": ["bogus"] })).await;
    assert!(error);
    let (error, text) = tool(&f.app, t, "export", json!({ "since": "yesterday" })).await;
    assert!(error && text.contains("RFC 3339"), "{text}");
    let (error, _) = tool(&f.app, t, "no_such_tool", json!({})).await;
    assert!(error);
}

#[tokio::test]
async fn app_paths_are_not_api_paths() {
    let f = fixture(Some(BASE)).await;
    let (status, _, _) = send(&f.app, get("/missing.js", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, headers, _) = send(&f.app, get("/sessions/claude-code/c1", None)).await;
    // The single-page app when it is built into the binary, a plain 503 when it is not.
    assert!(
        status == StatusCode::OK || status == StatusCode::SERVICE_UNAVAILABLE,
        "{status}"
    );
    if status == StatusCode::OK {
        assert!(
            headers
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
    }
}

// ---- rate limits per client address ----

/// The request as the listener hands it over when `peer` connects, with
/// these extra headers.
fn from(peer: &str, mut request: Request<Body>, headers: &[(&str, &str)]) -> Request<Body> {
    let addr = SocketAddr::new(peer.parse().unwrap(), 40_000);
    request.extensions_mut().insert(ConnectInfo(addr));
    for (name, value) in headers {
        request.headers_mut().append(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    request
}

async fn login_from(f: &Fixture, peer: &str, password: &str, headers: &[(&str, &str)]) -> StatusCode {
    let login = post_json("/api/auth/login", json!({ "password": password }), None);
    send(&f.app, from(peer, login, headers)).await.0
}

#[tokio::test]
async fn wrong_passwords_are_counted_per_client_address() {
    let f = fixture(Some(BASE)).await;
    sign_in(&f).await;
    for _ in 0..5 {
        assert_eq!(
            login_from(&f, "203.0.113.7", "wrong", &[]).await,
            StatusCode::UNAUTHORIZED
        );
    }
    let login = post_json("/api/auth/login", json!({ "password": PASSWORD }), None);
    let (status, _, body) = send(&f.app, from("203.0.113.7", login, &[])).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let wait = json_of(&body)["error"]["retry_after_secs"].as_u64().unwrap();
    assert!((1..=60).contains(&wait), "{wait}");
    // The same address written as IPv6 is the same client; another one is not.
    assert_eq!(
        login_from(&f, "::ffff:203.0.113.7", PASSWORD, &[]).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        login_from(&f, "198.51.100.7", PASSWORD, &[]).await,
        StatusCode::NO_CONTENT
    );
    // IPv6 is counted by /64.
    for _ in 0..5 {
        assert_eq!(
            login_from(&f, "2001:db8:1:2::1", "wrong", &[]).await,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        login_from(&f, "2001:db8:1:2::abcd", PASSWORD, &[]).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        login_from(&f, "2001:db8:1:3::1", PASSWORD, &[]).await,
        StatusCode::NO_CONTENT
    );

    // Wrong passwords on the consent page count with the login of the same address only.
    let client_id = register(&f, "http://localhost/callback").await;
    let html = consent_page(&f, &authorize_path(&client_id, REDIRECT, &[])).await;
    let approve = |peer: &str, password: &str| {
        let pairs = [
            ("request_id", hidden(&html, "request_id")),
            ("csrf", hidden(&html, "csrf")),
            ("password", password.to_string()),
            ("action", "approve".to_string()),
        ];
        let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        from(peer, form("/oauth/authorize", &pairs), &[])
    };
    for _ in 0..5 {
        let (status, _, page) = send(&f.app, approve("198.51.100.8", "not the password")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(String::from_utf8(page).unwrap().contains("密码不对"));
    }
    let (_, _, page) = send(&f.app, approve("198.51.100.8", PASSWORD)).await;
    assert!(String::from_utf8(page).unwrap().contains("尝试次数太多"));
    assert_eq!(
        login_from(&f, "198.51.100.8", PASSWORD, &[]).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    let (status, headers, _) = send(&f.app, approve("198.51.100.9", PASSWORD)).await;
    assert_eq!(status, StatusCode::FOUND);
    assert!(redirect_query(&headers).1.contains_key("code"));
}

#[tokio::test]
async fn wrong_setup_codes_are_counted_per_client_address() {
    let f = fixture(None).await;
    let setup = |code: &str| {
        post_json(
            "/api/auth/setup",
            json!({ "password": PASSWORD, "setup_code": code }),
            None,
        )
    };
    for _ in 0..5 {
        let (status, _, _) = send(&f.app, from("203.0.113.7", setup("WRONG-CODE"), &[])).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, _, _) = send(&f.app, from("203.0.113.7", setup(&f.setup_code), &[])).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let (status, _, _) = send(&f.app, from("198.51.100.7", setup(&f.setup_code), &[])).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn forwarded_addresses_are_believed_only_from_a_trusted_proxy() {
    let f = fixture_with(
        Some(BASE),
        &[("NEKORISU_TRUSTED_PROXIES", "192.0.2.0/24, 2001:db8:ffff::/48")],
    )
    .await;
    sign_in(&f).await;
    let real = |ip: &'static str| [("x-real-ip", ip)];

    // Through the proxy, as 203.0.113.7.
    for _ in 0..5 {
        let status = login_from(&f, "192.0.2.10", "wrong", &real("203.0.113.7")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        login_from(&f, "192.0.2.10", PASSWORD, &real("203.0.113.7")).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // The same client through another proxy, named by X-Forwarded-For.
    let chain = [("x-forwarded-for", "198.51.100.1, 203.0.113.7, 2001:db8:ffff::2")];
    assert_eq!(
        login_from(&f, "::ffff:192.0.2.11", PASSWORD, &chain).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Someone else behind the same proxy is not locked out, whatever they
    // wrote at the left of X-Forwarded-For.
    assert_eq!(
        login_from(&f, "192.0.2.10", PASSWORD, &real("203.0.113.8")).await,
        StatusCode::NO_CONTENT
    );
    let chain = [("x-forwarded-for", "203.0.113.7, 203.0.113.9")];
    assert_eq!(
        login_from(&f, "192.0.2.10", PASSWORD, &chain).await,
        StatusCode::NO_CONTENT
    );

    // A peer that is not a proxy cannot choose its address: it is counted as itself.
    for _ in 0..5 {
        let status = login_from(&f, "198.51.100.20", "wrong", &real("203.0.113.99")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        login_from(&f, "198.51.100.20", PASSWORD, &real("203.0.113.100")).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        login_from(&f, "192.0.2.10", PASSWORD, &real("203.0.113.99")).await,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn registrations_are_limited_per_client_address() {
    let f = fixture(Some(BASE)).await;
    let body = json!({ "client_name": "Loop", "redirect_uris": [REDIRECT] });
    let register_from = |peer: &str, body: &Value| from(peer, post_json("/oauth/register", body.clone(), None), &[]);
    for _ in 0..10 {
        assert_eq!(
            send(&f.app, register_from("203.0.113.7", &body)).await.0,
            StatusCode::CREATED
        );
    }
    let (status, headers, reply) = send(&f.app, register_from("203.0.113.7", &body)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json_of(&reply)["error"], "too_many_requests");
    let retry: u64 = headers[header::RETRY_AFTER].to_str().unwrap().parse().unwrap();
    assert!((1..=600).contains(&retry), "{retry}");
    // A bad registration stores nothing and is answered as before.
    let bad = json!({ "redirect_uris": ["myagent://cb"] });
    assert_eq!(
        send(&f.app, register_from("203.0.113.7", &bad)).await.0,
        StatusCode::BAD_REQUEST
    );
    // Another address registers.
    assert_eq!(
        send(&f.app, register_from("198.51.100.7", &body)).await.0,
        StatusCode::CREATED
    );
}
