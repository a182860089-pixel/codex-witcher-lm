use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SwitcherError {
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("configuration changed after it was inspected: {0}")]
    Conflict(String),
    #[error("refusing to access a symbolic link: {0}")]
    SymbolicLink(PathBuf),
    #[error("expected a regular file: {0}")]
    NotRegularFile(PathBuf),
    #[error("configuration is not valid UTF-8")]
    InvalidUtf8,
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml_edit::TomlError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("URL parse error: {0}")]
    Url(#[from] url::ParseError),
    #[error("CDP supervisor error: {0}")]
    Supervisor(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("atomic file replacement failed: {0}")]
    Persist(#[from] tempfile::PersistError),
    #[error("atomic file replacement failed: {0}")]
    AtomicReplace(String),
}

pub type Result<T> = std::result::Result<T, SwitcherError>;
