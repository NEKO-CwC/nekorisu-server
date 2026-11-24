//! OAuth 2.1 for the MCP endpoint, so that nobody copies a bearer token: the
//! agent opens a browser, the person types the panel's administrator password
//! on a page this server renders, and the agent gets its tokens.
//!
//! Follows the MCP authorization spec: protected resource metadata (RFC 9728)
//! points at this server as the authorization server (RFC 8414 metadata);
//! clients name themselves with a Client ID Metadata Document or register
//! (RFC 7591); PKCE with S256 only; tokens are bound to the canonical resource
//! `<public url>/mcp` (RFC 8707); the authorization response carries `iss`
//! (RFC 9207); refresh tokens rotate, and a reused one revokes its grant.
//!
//! The whole thing is off (404) when `NEKORISU_PUBLIC_URL` is not set: the
//! issuer and resource are derived from it, never from request headers.

pub mod client;
pub mod dns;
mod page;
mod store;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use subtle::ConstantTimeEq;

use crate::auth::AuthError;
use crate::client_ip::ClientIp;
use crate::limit::{Limiter, RateKey, Rule};
use crate::settings::Settings;
use crate::state::Shared;
use client::{Client, MetadataFetcher};
use store::{Db, NewGrant, NewTokens};

/// The one scope: read sessions, devices and exports.
pub const SCOPE: &str = "nekorisu:read";

/// How long the consent page stays valid.
const REQUEST_TTL: Duration = Duration::from_secs(10 * 60);
const CODE_TTL_MS: i64 = 5 * 60 * 1000;
pub const ACCESS_TTL_SECS: i64 = 3600;
const REFRESH_TTL_MS: i64 = 30 * 24 * 3600 * 1000;
/// Consent pages open at once; the oldest goes when there are more.
const MAX_PENDING: usize = 200;
const CLIENT_CACHE_OK: Duration = Duration::from_secs(10 * 60);
const CLIENT_CACHE_FAILED: Duration = Duration::from_secs(30);
const MAX_CACHED_CLIENTS: usize = 256;
const MAX_STATE_LEN: usize = 2048;
/// Registrations stored per client (an IPv4 address or an IPv6 /64) within
/// the window. An agent registers once per `mcp login`, so this leaves room
/// for retries and a few agents on one machine.
const REGISTRATIONS_PER_CLIENT: usize = 10;
const REGISTRATION_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Registrations from all clients together within the window: ten times one
/// client's allowance. The store keeps at most 500 registrations nobody has
/// approved and drops the oldest first (store.rs); at this rate a flood needs
/// at least 50 minutes to push one out, far longer than a consent page lives
/// (REQUEST_TTL), so it cannot push out a registration someone is approving.
const REGISTRATIONS_ALL: usize = 10 * REGISTRATIONS_PER_CLIENT;
const REGISTRATION_RULE: Rule = Rule {
    per_key: REGISTRATIONS_PER_CLIENT,
    global: REGISTRATIONS_ALL,
    window: REGISTRATION_WINDOW,
    // Above the ceiling, so it cannot fill with live clients (crate::limit).
    max_keys: 1024,
};

/// The addresses everything is published under, all derived from the public URL.
#[derive(Clone, Debug)]
pub struct Endpoints {
    /// The authorization server's issuer: the public URL itself.
    pub issuer: String,
    /// The MCP endpoint's canonical URI, which tokens are bound to.
    pub resource: String,
    pub resource_metadata: String,
}

impl Endpoints {
    pub fn new(public_url: &str) -> Self {
        let base = public_url.trim_end_matches('/');
        Self {
            issuer: base.to_string(),
            resource: format!("{base}/mcp"),
            resource_metadata: format!("{base}/.well-known/oauth-protected-resource/mcp"),
        }
    }
}

/// A consent page waiting for an answer.
#[derive(Clone)]
struct Pending {
    csrf: String,
    expires: Instant,
    client: Client,
    redirect_uri: String,
    state: Option<String>,
    code_challenge: String,
    resource: String,
    scope: String,
}

type Cached = (Instant, Result<Client, String>);

pub struct Oauth {
    db: Db,
    endpoints: Option<Endpoints>,
    metadata_documents: bool,
    fetcher: Arc<dyn MetadataFetcher>,
    pending: Mutex<HashMap<String, Pending>>,
    documents: Mutex<HashMap<String, Cached>>,
    registrations: Limiter,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}

fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// A PKCE verifier or challenge: 43 to 128 unreserved characters (RFC 7636).
fn pkce_shaped(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    if !pkce_shaped(verifier) {
        return false;
    }
    let digest = Sha256::digest(verifier.as_bytes());
    let computed = URL_SAFE_NO_PAD.encode(digest.as_slice());
    bool::from(computed.as_bytes().ct_eq(challenge.as_bytes()))
}

/// Whether a `resource` parameter names our MCP endpoint. Scheme and host
/// case and the default port are normalised by the URL parser; one trailing
/// slash is ignored.
fn is_our_resource(given: &str, canonical: &str) -> bool {
    let Ok(url) = url::Url::parse(given) else {
        return false;
    };
    if url.fragment().is_some() {
        return false;
    }
    let text = url.as_str();
    text.strip_suffix('/').unwrap_or(text) == canonical
}

/// What a request for `scope` is given: always the one scope. Unknown scopes
/// are refused; `offline_access` is accepted and needs nothing, since refresh
/// tokens are always issued.
fn granted_scope(requested: Option<&str>) -> Result<String, String> {
    for s in requested.unwrap_or_default().split(' ').filter(|s| !s.is_empty()) {
        if s != SCOPE && s != "offline_access" {
            return Err(format!("unknown scope {s:?}; this server has only {SCOPE}"));
        }
    }
    Ok(SCOPE.to_string())
}

/// Form or query parameters. Empty values count as absent, and a parameter
/// given twice is an error (RFC 6749 §3.1, §3.2).
struct Params(Vec<(String, String)>);

impl Params {
    fn parse(bytes: &[u8]) -> Self {
        Self(url::form_urlencoded::parse(bytes).into_owned().collect())
    }

    /// Whether any parameter is given more than once.
    fn duplicated(&self) -> bool {
        let mut seen = std::collections::HashSet::new();
        !self.0.iter().all(|(k, _)| seen.insert(k.as_str()))
    }

    fn one(&self, key: &str) -> Result<Option<String>, String> {
        let mut found = self.0.iter().filter(|(k, _)| k == key);
        let first = found.next().map(|(_, v)| v.clone());
        if found.next().is_some() {
            return Err(format!("{key} is given more than once"));
        }
        Ok(first.filter(|v| !v.is_empty()))
    }
}

impl Oauth {
    pub async fn new(
        pool: SqlitePool,
        settings: &Settings,
        fetcher: Arc<dyn MetadataFetcher>,
    ) -> Result<Self, sqlx::Error> {
        Ok(Self {
            db: Db::new(pool).await?,
            endpoints: settings.public_url.as_deref().map(Endpoints::new),
            metadata_documents: settings.oauth_metadata_documents,
            fetcher,
            pending: Mutex::new(HashMap::new()),
            documents: Mutex::new(HashMap::new()),
            registrations: Limiter::new("client registration", REGISTRATION_RULE),
        })
    }

    /// None when MCP is off.
    pub fn endpoints(&self) -> Option<&Endpoints> {
        self.endpoints.as_ref()
    }

    /// Checks the bearer token of an MCP request. None when it is good;
    /// otherwise the response to send instead: 404 when MCP is off, or 401
    /// with a challenge that tells the agent where to sign in.
    pub async fn bearer_denial(&self, headers: &HeaderMap) -> Option<Response> {
        let Some(ep) = &self.endpoints else {
            return Some(StatusCode::NOT_FOUND.into_response());
        };
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token.trim())
            .filter(|t| !t.is_empty());
        if let Some(token) = token {
            let now = now_ms();
            match self.db.access(&hash(token), now).await {
                Ok(Some(row)) if row.resource == ep.resource && row.scope.split(' ').any(|s| s == SCOPE) => {
                    if let Err(error) = self.db.touch(&row.grant_id, row.last_used_ms, now).await {
                        tracing::warn!(%error, "could not note when an app was last used");
                    }
                    return None;
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::error!(%error, "checking an access token failed");
                    return Some(StatusCode::INTERNAL_SERVER_ERROR.into_response());
                }
            }
        }
        let mut challenge = format!(
            "Bearer resource_metadata=\"{}\", scope=\"{SCOPE}\"",
            ep.resource_metadata
        );
        if token.is_some() {
            challenge.push_str(", error=\"invalid_token\"");
        }
        let mut response = (StatusCode::UNAUTHORIZED, "sign in with OAuth: see WWW-Authenticate").into_response();
        if let Ok(value) = HeaderValue::from_str(&challenge) {
            response.headers_mut().insert(header::WWW_AUTHENTICATE, value);
        }
        Some(response)
    }

    /// The apps that can still use the MCP endpoint, for the panel.
    pub async fn apps(&self) -> Result<Vec<Value>, sqlx::Error> {
        let rows = self.db.grants(now_ms()).await?;
        Ok(rows
            .into_iter()
            .map(|g| {
                let (redirect_host, loopback) = client::describe_redirect(&g.redirect_uri);
                json!({
                    "id": g.id,
                    "client_name": g.client_name,
                    "client_id": g.client_id,
                    "metadata_document": client::is_url_client_id(&g.client_id),
                    "redirect_host": redirect_host,
                    "loopback": loopback,
                    "scope": g.scope,
                    "created_ms": g.created_ms,
                    "last_used_ms": g.last_used_ms,
                })
            })
            .collect())
    }

    /// Revokes an app's grant: its tokens stop working at once.
    pub async fn revoke(&self, grant_id: &str) -> Result<bool, sqlx::Error> {
        self.db.revoke(grant_id).await
    }

    /// The client behind a client id. The error is shown on a page.
    async fn client(&self, id: &str) -> Result<Client, String> {
        if client::is_url_client_id(id) {
            return self.client_from_document(id).await;
        }
        match self.db.client(id).await {
            Ok(Some(client)) => Ok(client),
            Ok(None) => Err(
                "服务端不认识这个应用（client_id）。它可能是在服务端换库之前注册的：让应用重新登录（例如先 claude mcp logout 再 login）。"
                    .to_string(),
            ),
            Err(error) => {
                tracing::error!(%error, "reading a client failed");
                Err("服务端读数据库出错了。".to_string())
            }
        }
    }

    async fn client_from_document(&self, id: &str) -> Result<Client, String> {
        if !self.metadata_documents {
            return Err(
                "这台服务端关掉了用网址作为 client_id 的方式（NEKORISU_OAUTH_CLIENT_METADATA_DOCUMENTS=false）。"
                    .to_string(),
            );
        }
        let url = client::metadata_document_url(id)?;
        if let Some((at, result)) = self.documents.lock().expect("lock").get(id)
            && at.elapsed()
                < if result.is_ok() {
                    CLIENT_CACHE_OK
                } else {
                    CLIENT_CACHE_FAILED
                }
        {
            return result.clone();
        }
        let result = match self.fetcher.fetch(&url).await {
            Ok(body) => client::parse_metadata_document(id, &body),
            Err(why) => {
                tracing::info!(client_id = id, %why, "could not fetch a client metadata document");
                Err(
                    "取不到这个应用的说明文档（client_id 指向的网址）。稍后再试；一直这样的话，告诉管理员。"
                        .to_string(),
                )
            }
        };
        let mut cache = self.documents.lock().expect("lock");
        if cache.len() >= MAX_CACHED_CLIENTS {
            cache.retain(|_, (at, _)| at.elapsed() < CLIENT_CACHE_OK);
            if cache.len() >= MAX_CACHED_CLIENTS {
                cache.clear();
            }
        }
        cache.insert(id.to_string(), (Instant::now(), result.clone()));
        result
    }

    /// Keeps a consent request; returns its id and CSRF token.
    fn remember(&self, mut request: Pending) -> (String, String) {
        let id = random_hex(16);
        let csrf = random_hex(16);
        request.csrf = csrf.clone();
        request.expires = Instant::now() + REQUEST_TTL;
        let mut pending = self.pending.lock().expect("lock");
        let now = Instant::now();
        pending.retain(|_, p| p.expires > now);
        while pending.len() >= MAX_PENDING {
            let oldest = pending.iter().min_by_key(|(_, p)| p.expires).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => pending.remove(&k),
                None => break,
            };
        }
        pending.insert(id.clone(), request);
        (id, csrf)
    }

    /// The consent request with this id, if it is alive and the CSRF token is its own.
    fn waiting(&self, id: &str, csrf: &str) -> Option<Pending> {
        let pending = self.pending.lock().expect("lock");
        let found = pending.get(id)?;
        (found.expires > Instant::now() && bool::from(found.csrf.as_bytes().ct_eq(csrf.as_bytes())))
            .then(|| found.clone())
    }

    /// Takes the request away: it can be answered once.
    fn take(&self, id: &str) -> Option<Pending> {
        self.pending
            .lock()
            .expect("lock")
            .remove(id)
            .filter(|p| p.expires > Instant::now())
    }
}

// ---- metadata ----

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

/// `/.well-known/oauth-protected-resource[/mcp]` (RFC 9728).
pub async fn resource_metadata(State(s): State<Shared>) -> Response {
    let Some(ep) = s.oauth.endpoints() else {
        return not_found();
    };
    Json(json!({
        "resource": ep.resource,
        "authorization_servers": [ep.issuer],
        "scopes_supported": [SCOPE],
        "bearer_methods_supported": ["header"],
        "resource_name": "nekorisu-server",
    }))
    .into_response()
}

/// `/.well-known/oauth-authorization-server` (RFC 8414).
pub async fn server_metadata(State(s): State<Shared>) -> Response {
    let Some(ep) = s.oauth.endpoints() else {
        return not_found();
    };
    let mut metadata = json!({
        "issuer": ep.issuer,
        "authorization_endpoint": format!("{}/oauth/authorize", ep.issuer),
        "token_endpoint": format!("{}/oauth/token", ep.issuer),
        "registration_endpoint": format!("{}/oauth/register", ep.issuer),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": [SCOPE],
        "authorization_response_iss_parameter_supported": true,
    });
    if s.oauth.metadata_documents {
        metadata["client_id_metadata_document_supported"] = json!(true);
    }
    Json(metadata).into_response()
}

// ---- registration (RFC 7591) ----

fn registration_error(code: &str, description: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": code, "error_description": description })),
    )
        .into_response()
}

fn too_many_registrations(wait_secs: u64) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": "too_many_requests",
            "error_description": format!("too many registrations; try again in {wait_secs} seconds"),
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(wait_secs));
    response
}

/// `POST /oauth/register`: public clients only. Each registration is a new
/// client id; ones nobody approves are forgotten after a day. Registrations
/// that would be stored are limited per client address.
pub async fn register(State(s): State<Shared>, ClientIp(ip): ClientIp, body: Bytes) -> Response {
    if s.oauth.endpoints().is_none() {
        return not_found();
    }
    let Ok(Value::Object(meta)) = serde_json::from_slice::<Value>(&body) else {
        return registration_error("invalid_client_metadata", "the body must be a JSON object");
    };
    let uris: Vec<String> = match meta.get("redirect_uris").and_then(Value::as_array) {
        Some(list) => list.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        None => Vec::new(),
    };
    let listed = meta.get("redirect_uris").and_then(Value::as_array).map_or(0, Vec::len);
    if uris.is_empty() || uris.len() != listed || uris.len() > client::MAX_REDIRECT_URIS {
        return registration_error("invalid_redirect_uri", "redirect_uris must list 1 to 10 URIs");
    }
    if let Some(bad) = uris.iter().find(|u| !client::valid_redirect_uri(u)) {
        return registration_error(
            "invalid_redirect_uri",
            &format!("{bad}: only https, or http on localhost, 127.0.0.1 or [::1]"),
        );
    }
    let strings = |key: &str| -> Option<Vec<String>> {
        meta.get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
    };
    if let Some(grants) = strings("grant_types")
        && (!grants.iter().any(|g| g == "authorization_code")
            || grants.iter().any(|g| g != "authorization_code" && g != "refresh_token"))
    {
        return registration_error(
            "invalid_client_metadata",
            "grant_types: only authorization_code and refresh_token",
        );
    }
    if let Some(types) = strings("response_types")
        && types.iter().any(|t| t != "code")
    {
        return registration_error("invalid_client_metadata", "response_types: only code");
    }
    let name = meta
        .get("client_name")
        .and_then(Value::as_str)
        .map(client::clean_name)
        .unwrap_or_default();
    let client = Client {
        id: format!("nk-{}", random_hex(16)),
        name,
        redirect_uris: uris,
    };
    let place = match s.oauth.registrations.reserve(RateKey::of(ip)) {
        Ok(place) => place,
        Err(wait_secs) => return too_many_registrations(wait_secs),
    };
    let now = now_ms();
    if let Err(error) = s.oauth.db.insert_client(&client, now).await {
        place.give_back();
        tracing::error!(%error, "registering a client failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "server_error" })),
        )
            .into_response();
    }
    place.keep();
    tracing::info!(client_id = %client.id, "registered an OAuth client");
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": client.id,
            "client_id_issued_at": now / 1000,
            "client_name": client.name,
            "redirect_uris": client.redirect_uris,
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        })),
    )
        .into_response()
}

// ---- authorization ----

/// Where the answer goes once the client and redirect URI are known.
struct Answer<'a> {
    redirect_uri: &'a str,
    state: Option<&'a str>,
    issuer: &'a str,
}

impl Answer<'_> {
    fn send(&self, pairs: &[(&str, &str)]) -> Response {
        let Ok(mut url) = url::Url::parse(self.redirect_uri) else {
            return page::error(StatusCode::BAD_REQUEST, "返回地址无效。");
        };
        {
            let mut query = url.query_pairs_mut();
            for (k, v) in pairs {
                query.append_pair(k, v);
            }
            if let Some(state) = self.state {
                query.append_pair("state", state);
            }
            query.append_pair("iss", self.issuer);
        }
        let mut response = StatusCode::FOUND.into_response();
        if let Ok(value) = HeaderValue::from_str(url.as_str()) {
            response.headers_mut().insert(header::LOCATION, value);
        }
        response
    }

    fn error(&self, code: &str, description: &str) -> Response {
        self.send(&[("error", code), ("error_description", description)])
    }
}

/// `GET /oauth/authorize`: checks the request and shows the consent page.
/// Until the client and its redirect URI are known to be right, problems are
/// shown on a page; after that they go back to the client (RFC 6749 §4.1.2.1).
pub async fn authorize_page(State(s): State<Shared>, RawQuery(raw): RawQuery) -> Response {
    let Some(ep) = s.oauth.endpoints() else {
        return not_found();
    };
    let params = Params::parse(raw.as_deref().unwrap_or_default().as_bytes());
    let client_id = match params.one("client_id") {
        Ok(Some(id)) => id,
        _ => return page::error(StatusCode::BAD_REQUEST, "请求里没有 client_id（或者给了不止一个）。"),
    };
    let client = match s.oauth.client(&client_id).await {
        Ok(client) => client,
        Err(why) => return page::error(StatusCode::BAD_REQUEST, &why),
    };
    let redirect_uri = match params.one("redirect_uri") {
        Ok(Some(uri)) if client::redirect_matches(&client.redirect_uris, &uri) => uri,
        Ok(Some(_)) => {
            return page::error(
                StatusCode::BAD_REQUEST,
                "返回地址（redirect_uri）不是这个应用登记过的。为了安全，不会跳过去。",
            );
        }
        _ => return page::error(StatusCode::BAD_REQUEST, "请求里没有返回地址（redirect_uri）。"),
    };
    let state = match params.one("state") {
        Ok(state) if state.as_ref().is_none_or(|s| s.len() <= MAX_STATE_LEN) => state,
        _ => return page::error(StatusCode::BAD_REQUEST, "state 参数不对。"),
    };
    let answer = Answer {
        redirect_uri: &redirect_uri,
        state: state.as_deref(),
        issuer: &ep.issuer,
    };

    let get = |key: &str| params.one(key);
    let (response_type, challenge, method, resource, scope) = match (
        get("response_type"),
        get("code_challenge"),
        get("code_challenge_method"),
        get("resource"),
        get("scope"),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d), Ok(e)) => (a, b, c, d, e),
        _ => return answer.error("invalid_request", "a parameter is given more than once"),
    };
    match response_type.as_deref() {
        Some("code") => {}
        None => return answer.error("invalid_request", "response_type is required"),
        Some(_) => return answer.error("unsupported_response_type", "only response_type=code"),
    }
    let Some(challenge) = challenge.filter(|c| pkce_shaped(c)) else {
        return answer.error("invalid_request", "PKCE is required: code_challenge with S256");
    };
    if method.as_deref() != Some("S256") {
        return answer.error("invalid_request", "code_challenge_method must be S256");
    }
    let resource = match resource {
        None => ep.resource.clone(),
        Some(r) if is_our_resource(&r, &ep.resource) => ep.resource.clone(),
        Some(_) => {
            return answer.error(
                "invalid_target",
                &format!("this server issues tokens only for {}", ep.resource),
            );
        }
    };
    let scope = match granted_scope(scope.as_deref()) {
        Ok(scope) => scope,
        Err(why) => return answer.error("invalid_scope", &why),
    };
    match s.auth.initialized().await {
        Ok(true) => {}
        Ok(false) => return page::no_admin(),
        Err(error) => {
            tracing::error!(%error, "reading the administrator failed");
            return page::error(StatusCode::INTERNAL_SERVER_ERROR, "服务端读数据库出错了。");
        }
    }
    let (request_id, csrf) = s.oauth.remember(Pending {
        csrf: String::new(),
        expires: Instant::now(),
        client: client.clone(),
        redirect_uri: redirect_uri.clone(),
        state,
        code_challenge: challenge,
        resource,
        scope,
    });
    page::consent(&consent(&client, &redirect_uri, &request_id, &csrf, None))
}

fn consent<'a>(
    client: &'a Client,
    redirect_uri: &'a str,
    request_id: &'a str,
    csrf: &'a str,
    problem: Option<&'a str>,
) -> page::Consent<'a> {
    page::Consent {
        client_id: &client.id,
        client_name: &client.name,
        metadata_document: client.is_metadata_document(),
        redirect_uri,
        request_id,
        csrf,
        problem,
    }
}

/// `POST /oauth/authorize`: the answer from the consent page. Approving needs
/// the administrator password, checked by the same limiter as the panel login
/// (per client address).
pub async fn authorize_decision(State(s): State<Shared>, ClientIp(ip): ClientIp, body: Bytes) -> Response {
    let Some(ep) = s.oauth.endpoints() else {
        return not_found();
    };
    let form = Params::parse(&body);
    let field = |key: &str| form.one(key).ok().flatten().unwrap_or_default();
    let (request_id, csrf) = (field("request_id"), field("csrf"));
    let Some(request) = s.oauth.waiting(&request_id, &csrf) else {
        return page::error(
            StatusCode::BAD_REQUEST,
            "这个授权请求已经过期或者已经处理过了。回到应用重新登录。",
        );
    };
    let answer = Answer {
        redirect_uri: &request.redirect_uri,
        state: request.state.as_deref(),
        issuer: &ep.issuer,
    };
    match field("action").as_str() {
        "deny" => {
            s.oauth.take(&request_id);
            answer.error("access_denied", "the person declined")
        }
        "approve" => {
            let retry = |problem: String| {
                page::consent(&consent(
                    &request.client,
                    &request.redirect_uri,
                    &request_id,
                    &csrf,
                    Some(&problem),
                ))
            };
            match s.auth.check_password(&field("password"), RateKey::of(ip)).await {
                Ok(()) => {}
                Err(AuthError::BadCredentials) => return retry("密码不对。".to_string()),
                Err(AuthError::TooManyAttempts(secs)) => {
                    return retry(format!("尝试次数太多，请 {secs} 秒后再试。"));
                }
                Err(_) => return page::error(StatusCode::INTERNAL_SERVER_ERROR, "服务端出错了。"),
            }
            // Answered once: a second submit finds nothing.
            if s.oauth.take(&request_id).is_none() {
                return page::error(StatusCode::BAD_REQUEST, "这个授权请求已经处理过了。");
            }
            let code = format!("nkc_{}", random_hex(32));
            let grant_id = random_hex(16);
            let now = now_ms();
            let grant = NewGrant {
                id: &grant_id,
                client_id: &request.client.id,
                client_name: &request.client.name,
                redirect_uri: &request.redirect_uri,
                resource: &request.resource,
                scope: &request.scope,
            };
            if let Err(error) = s
                .oauth
                .db
                .approve(&grant, &hash(&code), &request.code_challenge, now + CODE_TTL_MS, now)
                .await
            {
                tracing::error!(%error, "storing an approval failed");
                return answer.error("server_error", "the server could not store the approval");
            }
            tracing::info!(client_id = %request.client.id, grant = %grant_id, "an app was authorised");
            answer.send(&[("code", &code)])
        }
        _ => page::error(StatusCode::BAD_REQUEST, "不认识的操作。"),
    }
}

// ---- tokens ----

fn token_error(status: StatusCode, code: &str, description: &str) -> Response {
    let mut response = (status, Json(json!({ "error": code, "error_description": description }))).into_response();
    no_store(&mut response);
    response
}

fn no_store(response: &mut Response) {
    let h = response.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

fn bad_request(description: &str) -> Response {
    token_error(StatusCode::BAD_REQUEST, "invalid_request", description)
}

fn invalid_grant(description: &str) -> Response {
    token_error(StatusCode::BAD_REQUEST, "invalid_grant", description)
}

fn server_error(error: sqlx::Error) -> Response {
    tracing::error!(%error, "the token endpoint failed");
    token_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", "the server failed")
}

/// Undoes form encoding (RFC 6749 §2.3.1 encodes the Basic user name this way).
fn form_decode(text: &str) -> String {
    url::form_urlencoded::parse(format!("x={text}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// The client id of a token request: in the body, or as the user name of
/// HTTP Basic (some clients send it there with an empty secret).
fn token_client_id(headers: &HeaderMap, params: &Params) -> Result<Option<String>, &'static str> {
    let from_body = params
        .one("client_id")
        .map_err(|_| "client_id is given more than once")?;
    let from_basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
        .and_then(|(_, b64)| B64.decode(b64.trim()).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|pair| pair.split_once(':').map(|(id, _)| form_decode(id)))
        .filter(|id| !id.is_empty());
    match (from_body, from_basic) {
        (Some(a), Some(b)) if a != b => Err("client_id differs between the body and the Authorization header"),
        (a, b) => Ok(a.or(b)),
    }
}

/// `POST /oauth/token`: the authorization_code and refresh_token grants.
pub async fn token(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(ep) = s.oauth.endpoints() else {
        return not_found();
    };
    let params = Params::parse(&body);
    if params.duplicated() {
        return bad_request("a parameter is given more than once");
    }
    let one = |key: &str| params.one(key).ok().flatten();
    let client_id = match token_client_id(&headers, &params) {
        Ok(Some(id)) => id,
        Ok(None) => return bad_request("client_id is required"),
        Err(why) => return bad_request(why),
    };
    if let Some(resource) = one("resource")
        && !is_our_resource(&resource, &ep.resource)
    {
        return token_error(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            &format!("this server issues tokens only for {}", ep.resource),
        );
    }
    let now = now_ms();
    match one("grant_type").as_deref() {
        Some("authorization_code") => {
            let (Some(code), Some(verifier), Some(redirect_uri)) =
                (one("code"), one("code_verifier"), one("redirect_uri"))
            else {
                return bad_request("code, code_verifier and redirect_uri are required");
            };
            let code_hash = hash(&code);
            let row = match s.oauth.db.code(&code_hash).await {
                Ok(Some(row)) => row,
                Ok(None) => return invalid_grant("unknown code"),
                Err(e) => return server_error(e),
            };
            if row.used_ms.is_some() {
                // A code used twice may have been stolen: what it gave is taken back.
                if let Err(e) = s.oauth.db.revoke(&row.grant_id).await {
                    return server_error(e);
                }
                tracing::warn!(grant = %row.grant_id, "an authorization code was used twice; its grant is revoked");
                return invalid_grant("the code was already used; the tokens issued for it are revoked");
            }
            if row.expires_ms <= now {
                return invalid_grant("the code has expired");
            }
            if row.client_id != client_id || row.redirect_uri != redirect_uri {
                return invalid_grant("the code was issued to another client or redirect_uri");
            }
            if row.resource != ep.resource {
                return token_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_target",
                    "the code is for another resource",
                );
            }
            if !pkce_matches(&verifier, &row.code_challenge) {
                return invalid_grant("code_verifier does not match code_challenge");
            }
            issue(&s, now, &row.grant_id, &row.scope, Retire::Code(&code_hash)).await
        }
        Some("refresh_token") => {
            let Some(refresh) = one("refresh_token") else {
                return bad_request("refresh_token is required");
            };
            let old_hash = hash(&refresh);
            let row = match s.oauth.db.refresh(&old_hash).await {
                Ok(Some(row)) => row,
                Ok(None) => return invalid_grant("unknown refresh token"),
                Err(e) => return server_error(e),
            };
            if row.client_id != client_id {
                return invalid_grant("the refresh token was issued to another client");
            }
            if row.used_ms.is_some() {
                // A retired refresh token came back: someone kept a copy.
                if let Err(e) = s.oauth.db.revoke(&row.grant_id).await {
                    return server_error(e);
                }
                tracing::warn!(grant = %row.grant_id, "a refresh token was used twice; its grant is revoked");
                return invalid_grant("the refresh token was already used; the grant is revoked");
            }
            if row.expires_ms <= now {
                return invalid_grant("the refresh token has expired");
            }
            if row.resource != ep.resource {
                return token_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_target",
                    "the grant is for another resource",
                );
            }
            if let Err(why) = granted_scope(one("scope").as_deref()) {
                return token_error(StatusCode::BAD_REQUEST, "invalid_scope", &why);
            }
            issue(&s, now, &row.grant_id, &row.scope, Retire::Refresh(&old_hash)).await
        }
        None => bad_request("grant_type is required"),
        Some(_) => token_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "only authorization_code and refresh_token",
        ),
    }
}

/// What a token request uses up (by hash): the code, or the refresh token.
enum Retire<'a> {
    Code(&'a str),
    Refresh(&'a str),
}

/// Makes a new access and refresh token and stores them, retiring what they
/// replace in the same transaction. When that is found already retired, two
/// requests raced with the same code or refresh token: the grant is revoked.
async fn issue(s: &Shared, now: i64, grant_id: &str, scope: &str, retire: Retire<'_>) -> Response {
    let access = format!("nka_{}", random_hex(32));
    let refresh = format!("nkr_{}", random_hex(32));
    let (access_hash, refresh_hash) = (hash(&access), hash(&refresh));
    let tokens = NewTokens {
        access_hash: &access_hash,
        access_expires_ms: now + ACCESS_TTL_SECS * 1000,
        refresh_hash: &refresh_hash,
        refresh_expires_ms: now + REFRESH_TTL_MS,
    };
    let stored = match retire {
        Retire::Code(code_hash) => s.oauth.db.redeem_code(code_hash, grant_id, &tokens, now).await,
        Retire::Refresh(old_hash) => s.oauth.db.rotate(old_hash, grant_id, &tokens, now).await,
    };
    match stored {
        Ok(true) => {}
        Ok(false) => {
            if let Err(e) = s.oauth.db.revoke(grant_id).await {
                return server_error(e);
            }
            return invalid_grant("this was already used; the grant is revoked");
        }
        Err(e) => return server_error(e),
    }
    let mut response = Json(json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": ACCESS_TTL_SECS,
        "refresh_token": refresh,
        "scope": scope,
    }))
    .into_response();
    no_store(&mut response);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_s256() {
        // RFC 7636 appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(pkce_shaped(verifier) && pkce_shaped(challenge));
        assert!(pkce_matches(verifier, challenge));
        assert!(!pkce_matches("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXX", challenge));
        assert!(!pkce_matches("short", challenge));
        assert!(!pkce_shaped(&"a".repeat(129)));
        assert!(!pkce_shaped(&format!("{}+", "a".repeat(50))));
    }

    #[test]
    fn resources_compare_after_normalising() {
        let ours = "https://nekorisu.example.com:8443/mcp";
        assert!(is_our_resource(ours, ours));
        assert!(is_our_resource("HTTPS://NEKORISU.example.com:8443/mcp/", ours));
        assert!(!is_our_resource("https://nekorisu.example.com/mcp", ours));
        assert!(!is_our_resource("https://nekorisu.example.com:8443/other", ours));
        assert!(!is_our_resource("https://nekorisu.example.com:8443/mcp#x", ours));
        assert!(!is_our_resource("not a url", ours));
        assert!(is_our_resource(
            "https://nekorisu.example.com:443/mcp",
            "https://nekorisu.example.com/mcp"
        ));
    }

    #[test]
    fn one_scope() {
        assert_eq!(granted_scope(None).unwrap(), SCOPE);
        assert_eq!(granted_scope(Some("")).unwrap(), SCOPE);
        assert_eq!(granted_scope(Some("nekorisu:read offline_access")).unwrap(), SCOPE);
        assert!(granted_scope(Some("nekorisu:write")).is_err());
    }

    #[test]
    fn parameters_given_twice_are_errors() {
        let p = Params::parse(b"a=1&b=&a=2&c=3");
        assert!(p.one("a").is_err());
        assert_eq!(p.one("b").unwrap(), None);
        assert_eq!(p.one("c").unwrap().as_deref(), Some("3"));
        assert_eq!(p.one("d").unwrap(), None);
    }

    #[test]
    fn endpoints_come_from_the_public_url() {
        let ep = Endpoints::new("https://nekorisu.example.com:8443");
        assert_eq!(ep.issuer, "https://nekorisu.example.com:8443");
        assert_eq!(ep.resource, "https://nekorisu.example.com:8443/mcp");
        assert_eq!(
            ep.resource_metadata,
            "https://nekorisu.example.com:8443/.well-known/oauth-protected-resource/mcp"
        );
    }
}
