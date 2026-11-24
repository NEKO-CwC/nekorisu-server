//! The panel, built by Vite (web/) and embedded in the binary. Unknown paths
//! that are not files get `index.html`, so React Router handles them.

use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

use crate::error::ApiError;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Assets;

fn asset(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Some(
        (
            [
                (header::CONTENT_TYPE, mime.essence_str().to_string()),
                (header::CACHE_CONTROL, cache.to_string()),
            ],
            file.data.into_owned(),
        )
            .into_response(),
    )
}

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path == "api" || path.starts_with("api/") {
        return ApiError::not_found("not_found", "no such endpoint").into_response();
    }
    if !path.is_empty()
        && let Some(found) = asset(path)
    {
        return found;
    }
    // A path that names a file (has an extension) but is missing is a 404;
    // anything else is a page of the single-page app.
    let last = path.rsplit('/').next().unwrap_or_default();
    if last.contains('.') {
        return StatusCode::NOT_FOUND.into_response();
    }
    asset("index.html").unwrap_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "the panel was not built into this binary (run `pnpm build` in web/, then build the server again)",
        )
            .into_response()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn api_paths_never_fall_back_to_the_app() {
        let response = serve("/api/nope".parse().unwrap()).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            serve("/missing.js".parse().unwrap()).await.status(),
            StatusCode::NOT_FOUND
        );
    }
}
