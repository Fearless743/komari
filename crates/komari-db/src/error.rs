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

    #[error("zip archive error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("invalid backup archive: {0}")]
    InvalidBackup(String),

    #[error("unsafe path in backup archive: {0}")]
    UnsafeBackupPath(String),

    #[error("backup limit exceeded: {0}")]
    BackupLimit(String),

    #[error("backup database integrity check failed: {0}")]
    BackupIntegrity(String),

    #[error("backup database schema is incompatible: {0}")]
    BackupSchema(String),

    #[error("a backup import is already in progress")]
    BackupInProgress,
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
