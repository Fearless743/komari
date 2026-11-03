//! Errors returned by DDNS providers and the synchronization service.

use thiserror::Error;

/// A DDNS failure.
#[derive(Debug, Error)]
pub enum DdnsError {
    /// A provider is not registered in the factory.
    #[error("ddns provider not found: {0}")]
    ProviderNotFound(String),

    /// The provider configuration is missing or has the wrong shape.
    #[error("invalid ddns configuration: {0}")]
    InvalidConfig(String),

    /// A required provider setting is empty.
    #[error("{0}")]
    NotConfigured(String),

    /// The record addressed by a provider call does not exist.
    #[error("ddns record not found: {0}")]
    RecordNotFound(String),

    /// An HTTP request failed before a response was received.
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// An HTTP endpoint returned a non-success status code.
    #[error("{service} returned status {status}: {body}")]
    HttpStatus {
        service: &'static str,
        status: u16,
        body: String,
    },

    /// A provider rejected the synchronization request.
    #[error("{0}")]
    Provider(String),

    /// Every retry attempt failed.
    #[error("ddns sync failed after {retries} retries: {source}")]
    RetryExhausted {
        retries: u32,
        #[source]
        source: Box<DdnsError>,
    },

    /// A database operation failed.
    #[error(transparent)]
    Db(#[from] komari_db::DbError),
}

impl DdnsError {
    /// Whether this error means a Cloudflare record lookup found nothing.
    pub fn is_record_not_found(&self) -> bool {
        matches!(self, Self::RecordNotFound(_))
    }
}
