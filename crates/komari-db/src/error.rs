//! Error types for the database layer.

use thiserror::Error;

/// Errors returned by [`crate::Db`] and its repositories.
#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{entity} not found: {id}")]
    NotFound { entity: &'static str, id: String },

    #[error("invalid data: {0}")]
    InvalidData(String),

    #[error("unsupported database type {0:?} (only sqlite is implemented)")]
    UnsupportedDatabase(String),

    #[error("the database is opened read-only")]
    ReadOnly,

    #[error("time value error: {0}")]
    Time(String),
}

impl DbError {
    /// Build a [`DbError::NotFound`] for a given entity and identifier.
    pub fn not_found(entity: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound {
            entity,
            id: id.into(),
        }
    }
}

/// Convenience result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, DbError>;
