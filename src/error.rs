use std::io;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("TOML encode error: {0}")]
    TomlEncode(#[from] toml::ser::Error),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("invalid state: {0}")]
    InvalidState(String),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("component unavailable: {0}")]
    Unavailable(String),

    #[error("history error: {0}")]
    History(String),

    #[error("notification error: {0}")]
    Notification(String),
}
