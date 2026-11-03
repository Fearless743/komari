//! Administrative HTTP API (`/api/admin/**`).
//!
//! Every handler requires [`Role::Admin`](komari_auth::Role::Admin). The
//! [`RequireAdmin`] extractor enforces that even when the router is merged
//! without the [`RequireRole`](komari_auth::RequireRole) middleware.
//!
//! Responses use the same envelope as the Go backend. Provider configurations
//! are returned through `komari-ddns::redact_addition`,
//! `komari-notify::redact_addition` or the local OAuth redactor; the database
//! always keeps the complete `addition` JSON so delivery keeps working.

mod backup;
pub(crate) mod clients;
mod clipboard;
mod logs;
mod notifications;
mod providers;
mod sessions;
mod settings;
mod theme;
mod two_factor;
mod update;

use axum::Router;
use axum::body::Bytes;
use axum::http::header::COOKIE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use komari_auth::{AuthLayer, RequireAdmin, RequireRole};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::response;
use crate::state::ApiState;

/// Routes owned by the admin API, without role layers.
///
/// Every handler still takes [`RequireAdmin`], so this router is safe to merge
/// on its own. Prefer [`admin_router_with_state`] when the router is assembled
/// as a standalone application.
pub fn admin_router() -> Router<ApiState> {
    Router::new()
        .merge(backup::routes())
        .merge(update::routes())
        .merge(settings::routes())
        .merge(providers::routes())
        .merge(clients::routes())
        .merge(sessions::routes())
        .merge(two_factor::routes())
        .merge(theme::routes())
        .merge(logs::routes())
        .merge(clipboard::routes())
        .merge(notifications::routes())
}

/// Admin router with B03's body-preserving auth layer and the admin role
/// middleware installed.
pub fn admin_router_with_state(state: ApiState) -> Router {
    let auth = state.auth.clone();
    admin_router()
        .layer(RequireRole::admin(auth.clone()))
        .layer(AuthLayer::new(auth))
        .with_state(state)
}

/// Audit log helper. API-key requests use B03's synthetic UUID.
pub(crate) fn audit(state: &ApiState, admin: &RequireAdmin, message: &str, level: &str) {
    let uuid = admin
        .0
        .user_uuid
        .clone()
        .unwrap_or_else(|| komari_auth::API_KEY_UUID.to_owned());
    if let Err(error) = state.db.logs().log("", &uuid, message, level) {
        tracing::warn!(%error, "failed to write audit log");
    }
}

/// Read one cookie value (Go `c.Cookie`).
pub(crate) fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name).then(|| value.to_owned())
        })
        .next()
}

/// Build a `Set-Cookie` value with the attributes the Go admin handlers use.
pub(crate) fn set_cookie(name: &str, value: &str, max_age: i64) -> String {
    format!("{name}={value}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Lax")
}

/// Clear a cookie (`Max-Age=-1`).
pub(crate) fn clear_cookie(name: &str) -> String {
    set_cookie(name, "", -1)
}

/// Decode a JSON request body, returning the Go-style 400 envelope on error.
#[allow(clippy::result_large_err)] // `Response` is the handler's natural error type.
pub(crate) fn parse_json<T: DeserializeOwned>(body: &Bytes, prefix: &str) -> Result<T, Response> {
    serde_json::from_slice(body).map_err(|error| {
        response::error_status(StatusCode::BAD_REQUEST, format!("{prefix}{error}"))
    })
}

/// Decode a JSON request body as a generic value.
#[allow(clippy::result_large_err)]
pub(crate) fn parse_value(body: &Bytes, prefix: &str) -> Result<Value, Response> {
    parse_json(body, prefix)
}

/// `500 {"status":"error","message":...}`.
pub(crate) fn internal(message: impl Into<String>) -> Response {
    response::error_status(StatusCode::INTERNAL_SERVER_ERROR, message)
}

/// `400 {"status":"error","message":...}`.
pub(crate) fn bad_request(message: impl Into<String>) -> Response {
    response::error_status(StatusCode::BAD_REQUEST, message)
}

/// `404 {"status":"error","message":...}`.
pub(crate) fn not_found(message: impl Into<String>) -> Response {
    response::error_status(StatusCode::NOT_FOUND, message)
}

/// `200 {"status":"success","message":"","data":null}`.
pub(crate) fn ok() -> Response {
    response::success(Value::Null).into_response()
}

pub(crate) use clients::client_patch;
