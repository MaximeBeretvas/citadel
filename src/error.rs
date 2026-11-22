use thiserror::Error;

/// Shared library error type for `citadel`.
#[derive(Debug, Error)]
pub enum VaultError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("invalid query: {0}")]
    InvalidQuery(String),

    #[error("query timed out: {0}")]
    QueryTimeout(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("already running: {0}")]
    AlreadyRunning(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("{0}")]
    Other(String),
}

/// Convenience result alias using [`VaultError`].
pub type Result<T> = std::result::Result<T, VaultError>;
