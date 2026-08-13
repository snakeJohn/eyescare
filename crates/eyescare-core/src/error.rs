//! core 错误模型。序列化为 `{ "code", "display_id"?, "message" }`。

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("platform error: {0}")]
    Platform(#[from] eyescare_platform::Error),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("unsupported schema version {found} (latest {latest})")]
    UnsupportedSchema { found: u32, latest: u32 },
    #[error("rule validation failed: {0}")]
    RuleValidation(String),
    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;
