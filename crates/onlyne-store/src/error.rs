use std::fmt;

pub const UNSUPPORTED_SCHEMA: &str = "onlyne: unsupported schema; v1.0.0 does not migrate";

pub type StoreResult<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    UnsupportedSchema(String),
    NotFound,
    Busy,
    Sqlite(String),
    Serialization(String),
    InvalidState { from: String, to: String },
}

impl StoreError {
    pub(crate) fn unsupported_schema() -> Self {
        StoreError::UnsupportedSchema(UNSUPPORTED_SCHEMA.to_string())
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::UnsupportedSchema(message) => f.write_str(message),
            StoreError::NotFound => f.write_str("not found"),
            StoreError::Busy => f.write_str("database busy"),
            StoreError::Sqlite(message) => write!(f, "sqlite: {message}"),
            StoreError::Serialization(message) => write!(f, "serialization: {message}"),
            StoreError::InvalidState { from, to } => {
                write!(f, "invalid ledger state transition {from} -> {to}")
            }
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(value: rusqlite::Error) -> Self {
        match &value {
            rusqlite::Error::SqliteFailure(err, _) => match err.code {
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                    StoreError::Busy
                }
                _ => StoreError::Sqlite(value.to_string()),
            },
            _ => StoreError::Sqlite(value.to_string()),
        }
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(value: serde_json::Error) -> Self {
        StoreError::Serialization(value.to_string())
    }
}

impl From<chrono::ParseError> for StoreError {
    fn from(value: chrono::ParseError) -> Self {
        StoreError::Serialization(value.to_string())
    }
}
