//! Errors returned by GeoIP providers.

use thiserror::Error;

/// A GeoIP lookup or database failure.
#[derive(Debug, Error)]
pub enum GeoIpError {
    /// No usable provider is configured.
    #[error("you are using an empty GeoIP provider, please set a valid provider")]
    EmptyProvider,

    /// The provider name is not registered.
    #[error("geoip provider not found: {0}")]
    ProviderNotFound(String),

    /// A MaxMind database has not been opened yet.
    #[error("MaxMind database is not initialized or failed to open")]
    DatabaseUnavailable,

    /// An IP address could not be parsed.
    #[error("invalid IP address: {0}")]
    InvalidIp(String),

    /// An HTTP request failed before a response was received.
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// A remote service returned a non-success status code.
    #[error("{service} returned status {status}: {body}")]
    HttpStatus {
        service: &'static str,
        status: u16,
        body: String,
    },

    /// A remote service answered with an application-level error.
    #[error("{0}")]
    Provider(String),

    /// Reading or querying a MaxMind database failed.
    #[error("maxminddb error: {0}")]
    MaxMind(#[from] maxminddb::MaxMindDbError),

    /// Database file operations failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A persisted configuration value could not be read.
    #[error("database error: {0}")]
    Db(#[from] komari_db::DbError),

    /// The provider configuration is invalid.
    #[error("invalid geoip configuration: {0}")]
    Config(String),
}
