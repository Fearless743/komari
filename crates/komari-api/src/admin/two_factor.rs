//! Two-factor authentication (`/api/admin/2fa/**`).
//!
//! Go returns a PNG QR code from `/2fa/generate`. Generating a QR image would
//! require a new image dependency, so this port returns the same enrolment
//! material (`secret` and `otpauth_url`) as JSON and stores the secret in the
//! same `2fa_secret` cookie. The enable/disable flow is unchanged.

use std::collections::HashMap;
use std::io::Cursor;

use axum::Router;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, SET_COOKIE};
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
    let code = match qrcode::QrCode::new(setup.otpauth_url.as_bytes()) {
        Ok(code) => code,
        Err(error) => return internal(format!("Failed to generate 2FA QR code: {error}")),
    };
    let image = code
        .render::<image::Luma<u8>>()
        .min_dimensions(256, 256)
        .build();
    let mut png = Cursor::new(Vec::new());
    if let Err(error) =
        image::DynamicImage::ImageLuma8(image).write_to(&mut png, image::ImageFormat::Png)
    {
        return internal(format!("Failed to encode 2FA QR code: {error}"));
    }

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "image/png")
        .body(Body::from(png.into_inner()))
        .expect("static PNG response is valid");
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
