//! Two-factor authentication (`/api/admin/2fa/**`).
//!
//! Go returns a PNG QR code from `/2fa/generate`. Generating a QR image would
//! require a new image dependency, so this port returns the same enrolment
//! material (`secret` and `otpauth_url`) as JSON and stores the secret in the
//! same `2fa_secret` cookie. The enable/disable flow is unchanged.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::HeaderValue;
use axum::http::header::SET_COOKIE;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use komari_auth::{RequireAdmin, disable_2fa, enable_2fa_with_code, generate_2fa};
use serde_json::json;

use super::{audit, bad_request, clear_cookie, cookie, internal, set_cookie};
use crate::response;
use crate::state::ApiState;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/admin/2fa/generate", get(generate))
        .route("/api/admin/2fa/enable", post(enable))
        .route("/api/admin/2fa/disable", post(disable))
}

/// `GET /api/admin/2fa/generate`.
pub(crate) async fn generate(_admin: RequireAdmin) -> Response {
    let setup = match generate_2fa() {
        Ok(setup) => setup,
        Err(error) => return internal(format!("Failed to generate 2FA: {error}")),
    };
    let mut response = response::success(json!({
        "secret": setup.secret,
        "otpauth_url": setup.otpauth_url,
    }))
    .into_response();
    if let Ok(value) = HeaderValue::from_str(&set_cookie("2fa_secret", &setup.secret, 1800)) {
        response.headers_mut().insert(SET_COOKIE, value);
    }
    response
}

/// `POST /api/admin/2fa/enable?code=123456`.
pub(crate) async fn enable(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    headers: axum::http::HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(secret) = cookie(&headers, "2fa_secret").filter(|value| !value.is_empty()) else {
        return bad_request("2FA secret or code not provided");
    };
    let Some(code) = query.get("code").filter(|value| !value.is_empty()) else {
        return bad_request("2FA secret or code not provided");
    };
    let Some(uuid) = admin.0.user_uuid.as_deref() else {
        return bad_request("2FA secret or code not provided");
    };

    match enable_2fa_with_code(&state.db, uuid, &secret, code) {
        Ok(true) => {
            audit(&state, &admin, "enable 2FA", "info");
            let mut response = response::success(json!("2FA enabled successfully")).into_response();
            if let Ok(value) = HeaderValue::from_str(&clear_cookie("2fa_secret")) {
                response.headers_mut().insert(SET_COOKIE, value);
            }
            response
        }
        Ok(false) => bad_request("Invalid 2FA code"),
        Err(error) => internal(format!("Failed to enable 2FA: {error}")),
    }
}

/// `POST /api/admin/2fa/disable`.
pub(crate) async fn disable(State(state): State<ApiState>, admin: RequireAdmin) -> Response {
    let Some(uuid) = admin.0.user_uuid.as_deref() else {
        return bad_request("2FA secret or code not provided");
    };
    match disable_2fa(&state.db, uuid) {
        Ok(_) => {
            audit(&state, &admin, "disable 2FA", "warn");
            response::success(json!("")).into_response()
        }
        Err(error) => internal(format!("Failed to disable 2FA: {error}")),
    }
}
