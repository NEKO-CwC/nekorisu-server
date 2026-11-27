//! The panel's HTTP interface (docs/api.md). Everything here reads; the only
//! writes are setting the password, signing in and out, and refreshing the
//! index (which lists the storage again and changes nothing in it).

use std::collections::HashMap;
use std::str::FromStr;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, RawQuery, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use nekorisu_archive::{Kind, LinesQuery, SessionFilter};
use nekorisu_cloud::{PanelForce, PathMapDoc, View, check_path_maps};
use nekorisu_export::{ExportRequest, Format, Scope, display_text, export_stream, select_fields};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{AuthError, clear_cookie, session_cookie, token_from_cookies};
use crate::client_ip::ClientIp;
use crate::error::{ApiError, ApiResult};
use crate::limit::RateKey;
use crate::state::Shared;
use crate::util::{cut, today_utc};

pub const SOURCE_URL: &str = env!("CARGO_PKG_REPOSITORY");
pub const COMMIT: &str = env!("NEKORISU_COMMIT");

// ---- helpers ----

fn session_token(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    token_from_cookies(cookies).map(str::to_owned)
}

fn param<'a>(q: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    q.get(key).map(String::as_str).filter(|v| !v.is_empty())
}

fn number<T: FromStr>(q: &HashMap<String, String>, key: &str, default: T) -> ApiResult<T> {
    match param(q, key) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| ApiError::bad_request("bad_param", format!("{key} must be a number"))),
    }
}

fn truthy(q: &HashMap<String, String>, key: &str) -> bool {
    matches!(param(q, key), Some("1" | "true" | "yes" | "on"))
}

fn auth_error(error: AuthError) -> ApiError {
    use axum::http::StatusCode as S;
    match error {
        AuthError::AlreadyInitialized => ApiError::new(
            S::CONFLICT,
            "already_initialized",
            "the administrator is already set up",
        ),
        AuthError::BadSetupCode => ApiError::new(S::FORBIDDEN, "bad_setup_code", "the setup code is not right"),
        AuthError::WeakPassword => ApiError::new(
            S::BAD_REQUEST,
            "weak_password",
            format!(
                "the password needs at least {} characters",
                crate::auth::MIN_PASSWORD_CHARS
            ),
        ),
        AuthError::BadCredentials => ApiError::new(S::UNAUTHORIZED, "bad_credentials", "wrong password"),
        AuthError::TooManyAttempts(secs) => ApiError::new(
            S::TOO_MANY_REQUESTS,
            "too_many_attempts",
            "too many wrong attempts; wait a moment",
        )
        .with("retry_after_secs", json!(secs)),
        AuthError::Internal(why) => ApiError::internal(why),
    }
}

fn signed_in(s: &Shared, token: &str) -> Response {
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, session_cookie(token, s.settings.cookie_secure))],
    )
        .into_response()
}

// ---- middleware ----

pub async fn require_login(State(s): State<Shared>, req: Request, next: Next) -> Response {
    let ok = match session_token(req.headers()) {
        Some(token) => s.auth.valid(&token).await,
        None => false,
    };
    if ok {
        next.run(req).await
    } else {
        ApiError::unauthenticated().into_response()
    }
}

pub async fn security_headers(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    let mut response = next.run(req).await;
    let h = response.headers_mut();
    // The OAuth consent page sets its own, stricter one.
    if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
                .parse()
                .expect("valid header"),
        );
    }
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().expect("valid header"));
    h.insert(header::X_FRAME_OPTIONS, "DENY".parse().expect("valid header"));
    h.insert(header::REFERRER_POLICY, "no-referrer".parse().expect("valid header"));
    if path.starts_with("/api/") || path == "/mcp" || path.starts_with("/oauth/") {
        h.insert(header::CACHE_CONTROL, "no-store".parse().expect("valid header"));
    }
    response
}

// ---- auth ----

pub async fn auth_state(State(s): State<Shared>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let initialized = s.auth.initialized().await?;
    let authenticated = match session_token(&headers) {
        Some(token) => s.auth.valid(&token).await,
        None => false,
    };
    Ok(Json(
        json!({ "initialized": initialized, "authenticated": authenticated }),
    ))
}

#[derive(Deserialize)]
pub struct SetupBody {
    password: String,
    setup_code: String,
}

pub async fn setup(
    State(s): State<Shared>,
    ClientIp(client): ClientIp,
    Json(body): Json<SetupBody>,
) -> ApiResult<Response> {
    let token = s
        .auth
        .setup(&body.password, &body.setup_code, RateKey::of(client))
        .await
        .map_err(auth_error)?;
    Ok(signed_in(&s, &token))
}

#[derive(Deserialize)]
pub struct LoginBody {
    password: String,
}

pub async fn login(
    State(s): State<Shared>,
    ClientIp(client): ClientIp,
    Json(body): Json<LoginBody>,
) -> ApiResult<Response> {
    let token = s
        .auth
        .login(&body.password, RateKey::of(client))
        .await
        .map_err(auth_error)?;
    Ok(signed_in(&s, &token))
}

pub async fn logout(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if let Some(token) = session_token(&headers) {
        s.auth.logout(&token).await;
    }
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, clear_cookie(s.settings.cookie_secure))],
    )
        .into_response()
}

// ---- overview ----

pub async fn overview(State(s): State<Shared>) -> ApiResult<Json<Value>> {
    let description = s.archive.provider().describe();
    let mut storage = json!({ "description": description, "ok": true, "error": null });
    let mut counts = json!({ "sessions": 0, "root_sessions": 0, "devices": 0, "bytes": 0, "by_agent": {} });
    let mut last_upload = Value::Null;
    let mut index_info = json!({ "refreshed_at_ms": null, "skipped_files": 0 });
    match s.archive.index().await {
        Ok(index) => {
            let by_agent: serde_json::Map<String, Value> = index
                .sessions_by_agent()
                .into_iter()
                .map(|(agent, n)| (agent, json!(n)))
                .collect();
            counts = json!({
                "sessions": index.session_count(),
                "root_sessions": index.root_session_count(),
                "devices": index.devices.len(),
                "bytes": index.total_bytes(),
                "by_agent": by_agent,
            });
            last_upload = json!(index.last_upload_ms());
            index_info = json!({ "refreshed_at_ms": index.refreshed_ms, "skipped_files": index.skipped });
            // An old index served while the storage is down is not "ok".
            if let Some(error) = s.archive.last_refresh_error() {
                storage = json!({ "description": description, "ok": false, "error": error });
            }
        }
        Err(error) => storage = json!({ "description": description, "ok": false, "error": error.to_string() }),
    }
    let config = s
        .store
        .latest(View::Account)
        .await?
        .map(|r| json!({ "serial": r.serial, "issued_at_ms": r.issued_at_ms, "etag": r.etag }));
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "commit": COMMIT,
        "source_url": SOURCE_URL,
        "storage": storage,
        "counts": counts,
        "last_upload_ms": last_upload,
        "config": config,
        "index": index_info,
    })))
}

pub async fn refresh(State(s): State<Shared>) -> ApiResult<Json<Value>> {
    let index = s.archive.refresh().await?;
    Ok(Json(
        json!({ "refreshed_at_ms": index.refreshed_ms, "sessions": index.session_count() }),
    ))
}

// ---- sessions ----

pub async fn sessions(State(s): State<Shared>, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let filter = SessionFilter {
        agent: param(&q, "agent").map(str::to_owned),
        device: param(&q, "device").map(str::to_owned),
        q: param(&q, "q").map(str::to_owned),
        include_children: truthy(&q, "children"),
        limit: number::<usize>(&q, "limit", 50)?.clamp(1, 200),
        offset: number(&q, "offset", 0)?,
    };
    let (total, items) = s.archive.sessions(&filter).await?;
    Ok(Json(json!({ "total": total, "items": items })))
}

pub async fn session(
    State(s): State<Shared>,
    Path((agent, id)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let detail = s
        .archive
        .session(&agent, &id, param(&q, "version"))
        .await
        .map_err(|e| match e {
            nekorisu_archive::Error::NotFound(what) => {
                ApiError::not_found("session_not_found", format!("not found: {what}"))
            }
            other => other.into(),
        })?;
    Ok(Json(serde_json::to_value(detail).map_err(ApiError::internal)?))
}

pub async fn lines(
    State(s): State<Shared>,
    Path((agent, id)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let mut kinds = Vec::new();
    for name in param(&q, "kinds")
        .unwrap_or_default()
        .split(',')
        .filter(|k| !k.is_empty())
    {
        kinds.push(
            Kind::parse(name).ok_or_else(|| ApiError::bad_request("bad_param", format!("unknown kind: {name}")))?,
        );
    }
    let query = LinesQuery {
        file: param(&q, "file").map(str::to_owned),
        from: param(&q, "from")
            .map(|v| v.parse())
            .transpose()
            .map_err(|_| ApiError::bad_request("bad_param", "from must be a number"))?,
        to: param(&q, "to")
            .map(|v| v.parse())
            .transpose()
            .map_err(|_| ApiError::bad_request("bad_param", "to must be a number"))?,
        kinds,
        select: None,
        limit: number::<usize>(&q, "limit", 100)?.clamp(1, 500),
        offset: number(&q, "offset", 0)?,
    };
    let max_chars: usize = number(&q, "max_field_chars", 4000)?;
    let want_raw = truthy(&q, "raw");
    let page = s
        .archive
        .lines(&agent, &id, param(&q, "version"), &query)
        .await
        .map_err(|e| match e {
            nekorisu_archive::Error::NotFound(what) if what.starts_with("session") => {
                ApiError::not_found("session_not_found", format!("not found: {what}"))
            }
            other => other.into(),
        })?;
    let items: Vec<Value> = page
        .items
        .iter()
        .map(|r| {
            let (text, truncated) = cut(&display_text(&agent, r.kind, &r.raw_type, &r.raw), max_chars);
            json!({
                "line": r.line,
                "kind": r.kind.as_str(),
                "raw_type": r.raw_type,
                "ts_ms": r.ts_ms,
                "text": text,
                "text_truncated": truncated,
                "raw_len": r.raw.len(),
                "raw": want_raw.then(|| String::from_utf8_lossy(&r.raw).into_owned()),
            })
        })
        .collect();
    Ok(Json(json!({
        "file": page.file,
        "version": page.version,
        "total_lines": page.total_lines,
        "matched": page.matched,
        "truncated": page.truncated,
        "items": items,
    })))
}

// ---- devices ----

pub async fn devices(State(s): State<Shared>) -> ApiResult<Json<Value>> {
    let stats = s.archive.devices().await.unwrap_or_default();
    let states: HashMap<String, nekorisu_cloud::DeviceState> = s.store.device_states().await?.into_iter().collect();
    let current = s.store.latest(View::Account).await?.map(|r| r.serial);

    let state_json = |id: &str| -> Value {
        match states.get(id) {
            None => Value::Null,
            Some(st) => json!({
                "verified": false,
                "received_ms": st.received_ms,
                "client_version": st.client_version,
                "applied_serial": st.applied_serial,
                "applied_etag": st.applied_etag,
                "behind": current.map(|c| c.saturating_sub(st.applied_serial)),
                "exec_allowed": st.exec_allowed,
                "local_values": st.local_values,
                "requests": st.requests,
                "last_failure": st.last_failure,
            }),
        }
    };
    let mut items: Vec<Value> = stats
        .iter()
        .map(|d| {
            json!({
                "id": d.id, "name": d.name, "first_seen_ms": d.first_seen_ms,
                "sessions": d.sessions, "bytes": d.bytes, "last_upload_ms": d.last_upload_ms,
                "state": state_json(&d.id),
            })
        })
        .collect();
    for id in states.keys().filter(|id| !stats.iter().any(|d| &d.id == *id)) {
        items.push(json!({
            "id": id, "name": null, "first_seen_ms": null,
            "sessions": 0, "bytes": 0, "last_upload_ms": null,
            "state": state_json(id),
        }));
    }
    Ok(Json(json!({ "items": items })))
}

// ---- config ----

pub async fn config(State(s): State<Shared>) -> ApiResult<Json<Value>> {
    let account = s.store.latest(View::Account).await?;
    let public = s.store.latest(View::Public).await?;
    let states = s.store.device_states().await?;
    let stats = s.archive.devices().await.unwrap_or_default();

    let mut every_device: Vec<String> = stats.iter().map(|d| d.id.clone()).collect();
    for (id, _) in &states {
        if !every_device.contains(id) {
            every_device.push(id.clone());
        }
    }
    let name_of = |id: &str| stats.iter().find(|d| d.id == id).and_then(|d| d.name.clone());
    let request_state = |device: &str, request: &str| -> &'static str {
        match states.iter().find(|(id, _)| id == device) {
            None => "unknown",
            Some((_, st)) => match st.requests.iter().find(|r| r.id == request).map(|r| r.state.as_str()) {
                Some("accepted") => "accepted",
                Some("rejected") => "rejected",
                Some(_) => "pending",
                None => "unknown",
            },
        }
    };

    let (current, requests) = match &account {
        None => (Value::Null, Vec::new()),
        Some(rev) => {
            let payload: Value = serde_json::from_slice(&rev.payload).map_err(ApiError::internal)?;
            let metas = s.store.resource_metas(&rev.resources).await?;
            let targets = |v: &Value| -> Vec<Value> {
                let listed: Vec<String> = v["devices"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|d| d.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default();
                let ids = if listed.is_empty() {
                    every_device.clone()
                } else {
                    listed
                };
                let id = v["id"].as_str().unwrap_or_default();
                ids.iter()
                    .map(|d| json!({ "id": d, "name": name_of(d), "state": request_state(d, id) }))
                    .collect()
            };
            let mut requests = Vec::new();
            for f in payload["force"].as_array().into_iter().flatten() {
                requests.push(json!({
                    "kind": "force", "id": f["id"], "action": null,
                    "changes": f["changes"].as_array().map(|c| c.iter().map(|c| json!({ "key": c["key"], "value_json": c["value_json"] })).collect::<Vec<_>>()).unwrap_or_default(),
                    "devices": targets(f),
                }));
            }
            for r in payload["requests"].as_array().into_iter().flatten() {
                let action = match r["action"].as_str() {
                    Some("EXEC_ACTION_ALLOW") => json!("allow"),
                    Some("EXEC_ACTION_REVOKE") => json!("revoke"),
                    _ => Value::Null,
                };
                requests.push(json!({
                    "kind": "exec", "id": r["id"], "action": action, "changes": [], "devices": targets(r),
                }));
            }
            let current = json!({
                "serial": rev.serial, "etag": rev.etag, "issued_at_ms": rev.issued_at_ms, "key_id": rev.key_id,
                "payload": payload,
                "resources": metas.iter().map(|m| json!({
                    "name": m.name, "version": m.version, "sha256": m.sha256,
                    "executable": m.executable, "size": m.size,
                })).collect::<Vec<_>>(),
            });
            (current, requests)
        }
    };
    let history: Vec<Value> = s
        .store
        .history(50)
        .await?
        .into_iter()
        .map(|h| json!({ "serial": h.serial, "view": h.view, "issued_at_ms": h.issued_at_ms, "etag": h.etag }))
        .collect();
    let source = s.source.read().expect("status lock").clone();
    let public_key = s.signer.public_key();
    let url = s
        .settings
        .device_url
        .clone()
        .or_else(|| s.settings.public_url.clone())
        .unwrap_or_else(|| "<server address>".to_string());
    Ok(Json(json!({
        "current": current,
        "public": { "enabled": s.settings.public_config, "serial": public.as_ref().map(|r| r.serial) },
        "signing": {
            "key_id": s.signer.key_id(),
            "public_key": public_key,
            "connect_command": format!("nekorisu source add home {url} --key {public_key}"),
        },
        "history": history,
        "requests": requests,
        "source": { "dir": source.dir, "checked_ms": source.checked_ms, "last_error": source.last_error },
        "device_token_set": s.settings.device_token.is_some(),
    })))
}

// ---- path maps ----

/// The path maps edited here, whether the newest revision carries them, and
/// each device's own rules as it last reported them.
pub async fn path_maps(State(s): State<Shared>) -> ApiResult<Json<Value>> {
    let (items, updated_ms) = s.store.path_maps().await?;
    let account = s.store.latest(View::Account).await?;
    let published: Option<Vec<PathMapDoc>> = account.as_ref().map(|rev| {
        serde_json::from_slice::<Value>(&rev.payload)
            .ok()
            .and_then(|v| serde_json::from_value(v["paths"]["map"].clone()).ok())
            .unwrap_or_default()
    });
    let stats = s.archive.devices().await.unwrap_or_default();
    let states: HashMap<String, nekorisu_cloud::DeviceState> = s.store.device_states().await?.into_iter().collect();
    let mut ids: Vec<String> = stats.iter().map(|d| d.id.clone()).collect();
    for id in states.keys() {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    let devices: Vec<Value> = ids
        .iter()
        .map(|id| {
            let state = states.get(id);
            json!({
                "id": id,
                "name": stats.iter().find(|d| &d.id == id).and_then(|d| d.name.clone()),
                "reported_ms": state.map(|st| st.received_ms),
                "applied_serial": state.map(|st| st.applied_serial),
                "local": state.map(|st| st.paths.clone()),
            })
        })
        .collect();
    let source = s.source.read().expect("status lock").clone();
    let force: Vec<Value> = s
        .store
        .panel_force()
        .await?
        .into_iter()
        .map(|f| {
            let state = states
                .get(&f.device)
                .and_then(|st| st.requests.iter().find(|r| r.id == f.id))
                .map(|r| r.state.clone())
                .unwrap_or_else(|| "unknown".to_string());
            json!({
                "id": f.id, "device": f.device, "from": f.from, "to": f.to,
                "created_ms": f.created_ms, "state": state,
            })
        })
        .collect();
    Ok(Json(json!({
        "items": items,
        "updated_ms": updated_ms,
        "published_serial": account.as_ref().map(|r| r.serial),
        "in_force": published.as_ref() == Some(&items),
        "publish_error": source.last_error,
        "devices": devices,
        "force": force,
    })))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Publishes now; says whether it did.
async fn publish_now(s: &Shared) -> ApiResult<Json<Value>> {
    let result = s.publisher.check().await;
    let serial = s.store.latest(View::Account).await?.map(|r| r.serial);
    Ok(Json(json!({
        "published": result.is_ok(),
        "serial": serial,
        "error": result.err().map(|e| e.to_string()),
    })))
}

#[derive(Deserialize)]
pub struct ForceBody {
    device: String,
    from: String,
    #[serde(default)]
    to: String,
}

/// Asks one device to replace (or, with an empty `to`, drop) a path rule it
/// locked. The device applies it only when its user accepts it there.
pub async fn force_path_map(State(s): State<Shared>, Json(body): Json<ForceBody>) -> ApiResult<Json<Value>> {
    let probe = PathMapDoc {
        from: body.from.clone(),
        to: body.to.clone(),
        devices: vec![body.device.clone()],
    };
    check_path_maps(std::slice::from_ref(&probe))
        .map_err(|e| ApiError::bad_request("invalid_path_maps", e.to_string()))?;
    let mut all = s.store.panel_force().await?;
    if all.len() >= 200 {
        return Err(ApiError::bad_request(
            "too_many_requests",
            "withdraw some force requests first",
        ));
    }
    let mut bytes = [0u8; 4];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut bytes);
    let id = format!("panel-{}", hex::encode(bytes));
    all.push(PanelForce {
        id: id.clone(),
        device: body.device,
        from: body.from,
        to: body.to,
        created_ms: now_ms(),
    });
    s.store.set_panel_force(&all, now_ms()).await?;
    tracing::info!(request = %id, "a force request for a path rule was made in the panel");
    let mut reply = publish_now(&s).await?;
    reply.0["id"] = json!(id);
    Ok(reply)
}

pub async fn withdraw_force(State(s): State<Shared>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let mut all = s.store.panel_force().await?;
    let before = all.len();
    all.retain(|f| f.id != id);
    if all.len() == before {
        return Err(ApiError::not_found("force_not_found", "no such force request"));
    }
    s.store.set_panel_force(&all, now_ms()).await?;
    tracing::info!(request = %id, "a force request was withdrawn in the panel");
    publish_now(&s).await
}

#[derive(Deserialize)]
pub struct PathMapsBody {
    items: Vec<PathMapDoc>,
}

/// Replaces the whole table and publishes it at once.
pub async fn save_path_maps(State(s): State<Shared>, Json(body): Json<PathMapsBody>) -> ApiResult<Json<Value>> {
    check_path_maps(&body.items).map_err(|e| ApiError::bad_request("invalid_path_maps", e.to_string()))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    s.store.set_path_maps(&body.items, now).await?;
    tracing::info!(entries = body.items.len(), "path maps saved in the panel");
    publish_now(&s).await
}

// ---- storage ----

pub async fn storage(State(s): State<Shared>) -> ApiResult<Json<Value>> {
    let provider = s.archive.provider();
    let started = std::time::Instant::now();
    let listing = provider.list(nekorisu_archive::ROOT).await;
    let latency = started.elapsed().as_millis() as u64;
    let (ok, error, layout) = match listing {
        Ok(entries) => (
            true,
            Value::Null,
            json!({ "found": true, "entries": entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>() }),
        ),
        Err(nekorisu_storage::Error::NotFound(_)) => {
            // The storage answers; it just has no nekorisu folder yet.
            (true, Value::Null, json!({ "found": false, "entries": [] }))
        }
        Err(e) => (false, json!(e.to_string()), json!({ "found": false, "entries": [] })),
    };
    // Whether the storage honors Range is learned by trying it, so try it on a
    // small file now: the answer should not wait for someone to read a session.
    if ok && provider.capabilities().range_read.is_none() {
        let devices = format!("{}/devices", nekorisu_archive::ROOT);
        if let Ok(files) = provider.list(&devices).await
            && let Some(file) = files.iter().find(|e| !e.is_dir && e.size > 1)
        {
            let _ = provider.read(&file.path, Some(0..1)).await;
        }
    }
    // What a device fills in (`nekorisu storage add`): the same address and
    // user name. The password is never returned.
    let dav = s.settings.webdav.as_ref();
    Ok(Json(json!({
        "description": provider.describe(),
        "address": dav.map(|d| d.for_devices().to_string()),
        "username": dav.and_then(|d| d.user.clone()),
        "ok": ok,
        "error": error,
        "latency_ms": latency,
        "range_read": provider.capabilities().range_read,
        "layout": layout,
    })))
}

// ---- activity ----

pub async fn activity(State(s): State<Shared>, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let limit: usize = number::<usize>(&q, "limit", 100)?.clamp(1, 1000);
    let index = s.archive.index().await?;
    let (records, skipped) = s.archive.activity(limit).await?;
    let items: Vec<Value> = records
        .iter()
        .map(|r| {
            json!({
                "at_ms": r.at_ms, "seq": r.seq, "device": index.device_ref(&r.device_id),
                "op": r.op, "sessions": r.sessions, "tags": r.tags, "note": r.note, "bytes": r.bytes,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "skipped_lines": skipped })))
}

// ---- export ----

pub async fn export_fields() -> Json<Value> {
    Json(json!({
        "formats": ["jsonl", "csv"],
        "fields": nekorisu_export::FIELDS.iter().map(|f| json!({ "name": f.name, "description": f.description })).collect::<Vec<_>>(),
    }))
}

/// `fields`, `format`, `agent`, `device`, `session` (repeatable), `since_ms`, `until_ms`.
pub fn export_request(pairs: &[(String, String)], max_rows: Option<usize>) -> ApiResult<ExportRequest> {
    let one = |key: &str| {
        pairs
            .iter()
            .find(|(k, v)| k == key && !v.is_empty())
            .map(|(_, v)| v.clone())
    };
    let time = |key: &str| -> ApiResult<Option<i64>> {
        one(key)
            .map(|v| v.parse::<i64>())
            .transpose()
            .map_err(|_| ApiError::bad_request("bad_param", format!("{key} must be a number of milliseconds")))
    };
    let format = match one("format") {
        None => Format::Jsonl,
        Some(f) => {
            Format::parse(&f).ok_or_else(|| ApiError::bad_request("bad_param", "format must be jsonl or csv"))?
        }
    };
    let requested: Vec<String> = one("fields")
        .map(|f| {
            f.split(',')
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(ExportRequest {
        scope: Scope {
            agent: one("agent"),
            device: one("device"),
            sessions: pairs
                .iter()
                .filter(|(k, v)| k == "session" && !v.is_empty())
                .map(|(_, v)| v.clone())
                .collect(),
            since_ms: time("since_ms")?,
            until_ms: time("until_ms")?,
        },
        fields: select_fields(&requested)?,
        format,
        max_rows,
    })
}

pub async fn export(State(s): State<Shared>, RawQuery(raw): RawQuery) -> ApiResult<Response> {
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(raw.as_deref().unwrap_or_default().as_bytes())
        .into_owned()
        .collect();
    let request = export_request(&pairs, None)?;
    let format = request.format;
    let mut stream = Box::pin(export_stream(s.archive.clone(), request));
    // Look at the first piece before answering, so a storage that is down gives an error, not a broken download.
    let first = stream.next().await.transpose().map_err(ApiError::from)?;
    let body = Body::from_stream(
        futures::stream::iter(first.map(Ok)).chain(stream.map(|r| r.map_err(|e| std::io::Error::other(e.to_string())))),
    );
    Ok((
        [
            (header::CONTENT_TYPE, format.content_type().to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!(
                    "attachment; filename=\"nekorisu-export-{}.{}\"",
                    today_utc(),
                    format.extension()
                ),
            ),
        ],
        body,
    )
        .into_response())
}

// ---- authorised apps (OAuth) ----

pub async fn oauth_apps(State(s): State<Shared>) -> ApiResult<Json<Value>> {
    let items = s.oauth.apps().await?;
    Ok(Json(json!({
        "mcp_url": s.oauth.endpoints().map(|ep| ep.resource.clone()),
        "items": items,
    })))
}

pub async fn revoke_oauth_app(State(s): State<Shared>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    if s.oauth.revoke(&id).await? {
        tracing::info!(grant = %id, "an app's authorisation was revoked in the panel");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("app_not_found", "no such authorised app"))
    }
}

pub async fn api_not_found() -> ApiError {
    ApiError::not_found("not_found", "no such endpoint")
}
