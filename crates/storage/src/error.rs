pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("the storage refused our credentials")]
    Unauthorized,
    #[error("invalid path: {0}")]
    InvalidPath(String),
    /// A response was bigger than the limit set for this call.
    #[error("response too large: {0}")]
    TooLarge(String),
    /// Anything else: network errors, unexpected statuses. Never carries a URL
    /// with credentials in it.
    #[error("storage error: {0}")]
    Other(String),
}
