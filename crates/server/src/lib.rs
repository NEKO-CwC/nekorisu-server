//! nekorisu-server: the panel, the MCP endpoint and the device protocol in one
//! process.

pub mod api;
pub mod auth;
pub mod client_ip;
pub mod db;
pub mod error;
pub mod limit;
pub mod mcp;
pub mod oauth;
pub mod settings;
pub mod state;
pub mod storage;
pub mod util;
pub mod web;

use axum::Router;
use axum::middleware;
use axum::routing::{get, post};

pub use state::{AppState, Shared};

/// All HTTP routes.
pub fn router(state: Shared) -> Router {
    let protected = Router::new()
        .route("/overview", get(api::overview))
        .route("/refresh", post(api::refresh))
        .route("/sessions", get(api::sessions))
        .route("/sessions/{agent}/{id}", get(api::session))
        .route("/sessions/{agent}/{id}/lines", get(api::lines))
        .route("/devices", get(api::devices))
        .route("/config", get(api::config))
        .route("/storage", get(api::storage))
        .route("/activity", get(api::activity))
        .route("/export/fields", get(api::export_fields))
        .route("/export", get(api::export))
        .route("/oauth/apps", get(api::oauth_apps))
        .route("/oauth/apps/{id}/revoke", post(api::revoke_oauth_app))
        .layer(middleware::from_fn_with_state(state.clone(), api::require_login));
    let open = Router::new()
        .route("/auth/state", get(api::auth_state))
        .route("/auth/setup", post(api::setup))
        .route("/auth/login", post(api::login))
        .route("/auth/logout", post(api::logout));
    Router::new()
        .nest("/api", open.merge(protected).fallback(api::api_not_found))
        .route("/mcp", post(mcp::handle))
        .route("/.well-known/oauth-protected-resource", get(oauth::resource_metadata))
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth::resource_metadata),
        )
        .route("/.well-known/oauth-authorization-server", get(oauth::server_metadata))
        .route("/oauth/register", post(oauth::register))
        .route(
            "/oauth/authorize",
            get(oauth::authorize_page).post(oauth::authorize_decision),
        )
        .route("/oauth/token", post(oauth::token))
        .route("/healthz", get(|| async { "ok" }))
        .fallback(web::serve)
        .layer(middleware::from_fn(api::security_headers))
        .with_state(state)
}
