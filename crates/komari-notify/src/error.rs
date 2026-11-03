//! Error type shared by message senders and the notification logic.

use thiserror::Error;

/// Errors produced while building or delivering a notification.
#[derive(Debug, Error)]
pub enum NotifyError {
    /// The provider is not fully configured (missing required fields).
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// The provider name is not registered in the factory.
    #[error("message sender provider not found: {0}")]
    ProviderNotFound(String),

    /// A provider cannot be implemented with the same semantics as Go.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// A message could not be sent.
    #[error("message send failed: {0}")]
    Send(String),

    /// An HTTP request failed before a response was received.
    #[error("http request failed: {0}")]
    Http(String),

    /// A remote endpoint returned an error status or payload.
    #[error("remote endpoint rejected the request ({status}): {body}")]
    Rejected { status: u16, body: String },

    /// A payload could not be serialized.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// The literal message was empty where the provider requires content.
    #[error("message is empty")]
    EmptyMessage,

    /// The configured SQLite database returned an error.
    #[error("database error: {0}")]
    Database(#[from] komari_db::DbError),

    /// SMTP message construction or delivery failed.
    #[error("email error: {0}")]
    Email(String),

    /// A notification was attempted before a provider was loaded.
    #[error("message sender provider is not initialized")]
    NotInitialized,
}

/// Convenience result alias for this crate.
pub type Result<T> = std::result::Result<T, NotifyError>;
