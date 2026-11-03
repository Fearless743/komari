//! Authentication, authorization and session management for Komari.
//!
//! This crate is the Rust port of the Go backend's
//! `database/accounts`, `database/accounts/2fa.go` and `api/Auth.go`.
//!
//! * [`password`] — bcrypt password hashing (with legacy SHA-256 fallback)
//! * [`session`] — login sessions stored in `komari-db`
//! * [`totp`] — RFC 6238 TOTP, compatible with `github.com/pquerna/otp`
//! * [`role`] — the `Admin` / `Client` / `Guest` role model
//! * [`middleware`] — axum extractor and `RequireRole` middleware

pub mod error;
pub mod middleware;
pub mod password;
pub mod role;
pub mod session;
pub mod totp;

pub use error::{AuthError, Result};
pub use middleware::{
    API_KEY_UUID, AuthContext, AuthState, RequireAdmin, RequireAuthenticated, RequireClient,
    RequireRole, RequireRoleService, SESSION_COOKIE_NAME, Unauthorized, authenticate_request,
    unauthorized_response,
};
pub use password::{
    DEFAULT_BCRYPT_COST, authenticate, change_password, change_password_by_uuid, hash_password,
    hash_password_with_cost, is_bcrypt_hash, legacy_hash_password, password_hash_cost,
    verify_legacy_password, verify_password,
};
pub use role::Role;
pub use session::{
    DEFAULT_SESSION_TTL_SECONDS, SessionValidation, create_session, create_session_with_expiry,
    delete_all_sessions, delete_expired_sessions, delete_session, require_session, session_user,
    validate_session, validate_session_with,
};
pub use totp::{
    TOTP_ACCOUNT_NAME, TOTP_DIGITS, TOTP_ISSUER, TOTP_PERIOD, TOTP_SKEW, TwoFactorSetup,
    disable_2fa, disable_all_2fa, enable_2fa, enable_2fa_with_code, generate_2fa, generate_code_at,
    generate_code_at_with, generate_secret, otpauth_url, validate_code, verify_2fa, verify_code_at,
    verify_code_at_with,
};
