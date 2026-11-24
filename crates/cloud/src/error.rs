pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The config directory has something wrong with it; the text says what.
    #[error("{0}")]
    Config(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("signing key: {0}")]
    Key(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    pub(crate) fn config(text: impl Into<String>) -> Self {
        Error::Config(text.into())
    }
}
