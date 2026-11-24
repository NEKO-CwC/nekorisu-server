use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use nekorisu_archive::Error as ArchiveError;
use nekorisu_export::ExportError;
use serde_json::{Value, json};

/// `{"error": {"code", "message", ...}}` (docs/api.md). `code` is what the
/// panel branches on; `message` is for people.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub extra: Option<Box<(&'static str, Value)>>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            extra: None,
        }
    }

    pub fn unauthenticated() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthenticated", "sign in first")
    }

    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    pub fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(%error, "request failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "something went wrong on the server",
        )
    }

    pub fn with(mut self, key: &'static str, value: Value) -> Self {
        self.extra = Some(Box::new((key, value)));
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(extra) = self.extra {
            let (key, value) = *extra;
            error[key] = value;
        }
        (self.status, Json(json!({ "error": error }))).into_response()
    }
}

impl From<ArchiveError> for ApiError {
    fn from(error: ArchiveError) -> Self {
        match &error {
            ArchiveError::NotFound(what) => ApiError::not_found("not_found", format!("not found: {what}")),
            ArchiveError::BadRequest(why) => ApiError::bad_request("bad_request", why.clone()),
            ArchiveError::LineMapMissing(_) => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "linemap_missing",
                "the storage has no line map for this part of the session",
            ),
            ArchiveError::Format { .. } => ApiError::new(StatusCode::BAD_GATEWAY, "bad_layout", error.to_string()),
            ArchiveError::Storage(_) => {
                tracing::warn!(%error, "storage failed");
                ApiError::new(StatusCode::BAD_GATEWAY, "storage_unavailable", error.to_string())
            }
        }
    }
}

impl From<ExportError> for ApiError {
    fn from(error: ExportError) -> Self {
        match error {
            ExportError::Archive(e) => e.into(),
            ExportError::BadRequest(why) => ApiError::bad_request("bad_request", why),
        }
    }
}

impl From<nekorisu_cloud::Error> for ApiError {
    fn from(error: nekorisu_cloud::Error) -> Self {
        ApiError::internal(error)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        ApiError::internal(error)
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
