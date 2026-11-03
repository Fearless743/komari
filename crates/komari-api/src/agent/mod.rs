//! Agent route handlers and shared authentication helpers.

pub mod basic_info;
pub mod register;
pub mod report;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use komari_auth::{AuthError, RequireRoleError, Role};
use serde_json::json;

use crate::response::error_json;
use crate::state::ApiState;

pub(crate) enum AuthFailure {
    Unauthorized,
    Backend(String),
}

/// Authenticate an agent token and require the `client` role.
pub(crate) fn authenticate(
    state: &ApiState,
    token: &str,
) -> Result<komari_auth::AgentIdentity, AuthFailure> {
    match komari_auth::require_role(&state.db, Some(token), &[Role::Client]) {
        Ok(identity) => Ok(identity),
        Err(RequireRoleError::Backend(error)) => Err(AuthFailure::Backend(error.to_string())),
        Err(RequireRoleError::Auth(
            AuthError::Missing | AuthError::Invalid | AuthError::Forbidden,
        )) => Err(AuthFailure::Unauthorized),
    }
}

pub(crate) fn auth_failure_response(failure: AuthFailure) -> Response {
    match failure {
        AuthFailure::Unauthorized => unauthorized(),
        AuthFailure::Backend(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_json(format!("Failed to authenticate client: {error}")),
        )
            .into_response(),
    }
}

pub(crate) fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({"status": "error", "message": "Unauthorized."})),
    )
        .into_response()
}
