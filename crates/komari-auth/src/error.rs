//! Errors returned by the authentication crate.

use thiserror::Error;

/// Authentication/authorization errors.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("database error: {0}")]
    Database(#[from] komari_db::DbError),

    #[error("bcrypt error: {0}")]
    Bcrypt(#[from] bcrypt::BcryptError),

    #[error("user not found: {0}")]
    UserNotFound(String),

    #[error("no user account exists")]
    NoUsers,

    #[error("invalid TOTP secret: {0}")]
    InvalidTotpSecret(String),

    #[error("invalid role: {0}")]
    InvalidRole(String),

    #[error("invalid TOTP code")]
    InvalidTotpCode,

    #[error("2FA is not enabled for user {0}")]
    TwoFactorNotEnabled(String),
}

/// Convenience result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, AuthError>;
