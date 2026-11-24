//! The server's HTTP surface end to end, with the real router, a synthetic
//! archive in memory, an in-memory database and a real config directory.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use nekorisu_archive::Archive;
use nekorisu_cloud::{DeviceState, FileSigner, Publisher, Signer, Store};
use nekorisu_server::auth::Auth;
use nekorisu_server::settings::Settings;
use nekorisu_server::state::AppState;
use nekorisu_server::{Shared, router};
use nekorisu_testkit::{ArchiveBuilder, FileSpec, SessionSpec, Usage, claude, codex};
use serde_json::{Value, json};
use sqlx::sqlite::SqlitePoolOptions;
use tower::ServiceExt;

const T: i64 = 1_794_700_800_000;
const PASSWORD: &str = "correct horse battery";

struct Fixture {
    app: Router,
    state: Shared,
    setup_code: String,
    _dir: tempfile::TempDir,
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

async fn fixture(mcp_token: Option<&str>) -> Fixture {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let store = Store::new(pool.clone());
    store.migrate().await.unwrap();
    let auth = Auth::new(pool).await.unwrap();
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
            "NEKORISU_MCP_TOKEN" => mcp_token.map(str::to_owned),
            "NEKORISU_DEVICE_TOKEN" => Some("device-token".to_string()),
            "NEKORISU_PUBLIC_URL" => Some("https://nekorisu.example".to_string()),
            _ => None,
        }
    };
    let settings = Settings::from_env(&env).unwrap();
    let state: Shared = Arc::new(AppState {
        settings,
        archive: Arc::new(archive()),
        store,
        auth,
        signer,
        source: publisher.status(),
    });
    Fixture {
        app: router(state.clone()),
        state,
        setup_code,
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
    let f = fixture(None).await;
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
    let f = fixture(None).await;
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
    let f = fixture(None).await;
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
    let f = fixture(None).await;
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
    let f = fixture(None).await;
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

async fn tool(app: &Router, name: &str, args: Value) -> (bool, String) {
    let request =
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } });
    let (status, _, body) = send(app, mcp(Some("mcp-secret"), request)).await;
    assert_eq!(status, StatusCode::OK);
    let reply = json_of(&body);
    let result = &reply["result"];
    (
        result["isError"] == true,
        result["content"][0]["text"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn mcp_is_off_without_a_token_and_guarded_with_one() {
    let off = fixture(None).await;
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    assert_eq!(
        send(&off.app, mcp(Some("anything"), ping.clone())).await.0,
        StatusCode::NOT_FOUND
    );

    let on = fixture(Some("mcp-secret")).await;
    assert_eq!(send(&on.app, mcp(None, ping.clone())).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        send(&on.app, mcp(Some("wrong"), ping.clone())).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, _, body) = send(&on.app, mcp(Some("mcp-secret"), ping)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&body)["result"], json!({}));

    let init =
        json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } });
    let (_, _, body) = send(&on.app, mcp(Some("mcp-secret"), init)).await;
    let reply = json_of(&body);
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(reply["result"]["serverInfo"]["name"], "nekorisu-server");

    let (_, _, body) = send(
        &on.app,
        mcp(
            Some("mcp-secret"),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
        ),
    )
    .await;
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
    assert_eq!(
        send(&on.app, mcp(Some("mcp-secret"), note)).await.0,
        StatusCode::ACCEPTED
    );
    let (_, _, body) = send(
        &on.app,
        mcp(
            Some("mcp-secret"),
            json!({ "jsonrpc": "2.0", "id": 4, "method": "nope" }),
        ),
    )
    .await;
    assert_eq!(json_of(&body)["error"]["code"], -32601);
}

#[tokio::test]
async fn mcp_tools_read_sessions_lines_and_exports() {
    let f = fixture(Some("mcp-secret")).await;

    let (error, text) = tool(&f.app, "list_sessions", json!({ "query": "synthetic" })).await;
    assert!(!error);
    assert!(text.starts_with("1 of 1 sessions\n"), "{text}");
    assert!(text.contains("claude-code:c1") && text.contains("mac-mini"));

    // A bare id works when it is unique.
    let (error, text) = tool(&f.app, "get_session", json!({ "session": "c1" })).await;
    assert!(!error, "{text}");
    let session = json_of(text.as_bytes());
    assert_eq!(session["session"], "claude-code:c1");
    assert_eq!(session["files"][0]["lines"], 5);

    let (error, text) = tool(
        &f.app,
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

    let (error, text) = tool(&f.app, "list_devices", json!({})).await;
    assert!(!error && text.contains("mac-mini"), "{text}");

    let (error, text) = tool(
        &f.app,
        "export",
        json!({ "fields": ["turn", "user"], "agent": "claude-code" }),
    )
    .await;
    assert!(!error, "{text}");
    assert_eq!(text.lines().count(), 2);
    assert!(text.lines().next().unwrap().contains("first question"));

    // Mistakes are tool errors the agent can read, not protocol errors.
    let (error, text) = tool(&f.app, "read_lines", json!({ "session": "nonexistent" })).await;
    assert!(error && text.contains("no session"), "{text}");
    let (error, _) = tool(&f.app, "read_lines", json!({ "session": "c1", "kinds": ["bogus"] })).await;
    assert!(error);
    let (error, text) = tool(&f.app, "export", json!({ "since": "yesterday" })).await;
    assert!(error && text.contains("RFC 3339"), "{text}");
    let (error, _) = tool(&f.app, "no_such_tool", json!({})).await;
    assert!(error);
}

#[tokio::test]
async fn app_paths_are_not_api_paths() {
    let f = fixture(None).await;
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
