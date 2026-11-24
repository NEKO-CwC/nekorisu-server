//! nekorisu-server: the panel, the MCP endpoint and the device protocol in one
//! process.

pub mod api;
pub mod auth;
pub mod db;
pub mod error;
pub mod mcp;
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
        .layer(middleware::from_fn_with_state(state.clone(), api::require_login));
    let open = Router::new()
        .route("/auth/state", get(api::auth_state))
        .route("/auth/setup", post(api::setup))
        .route("/auth/login", post(api::login))
        .route("/auth/logout", post(api::logout));
    Router::new()
        .nest("/api", open.merge(protected).fallback(api::api_not_found))
        .route("/mcp", post(mcp::handle))
        .route("/healthz", get(|| async { "ok" }))
        .fallback(web::serve)
        .layer(middleware::from_fn(api::security_headers))
        .with_state(state)
}
