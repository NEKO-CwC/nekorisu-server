use nekorisu_storage as storage;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Storage(#[from] storage::Error),
    #[error("not found: {0}")]
    NotFound(String),
    /// A file is there but is not what the layout says it should be.
    #[error("unreadable {what}: {why}")]
    Format { what: String, why: String },
    #[error("the line map for {0} is missing")]
    LineMapMissing(String),
    #[error("bad request: {0}")]
    BadRequest(String),
}

impl Error {
    pub(crate) fn format(what: impl Into<String>, why: impl Into<String>) -> Self {
        Error::Format {
            what: what.into(),
            why: why.into(),
        }
    }
}
