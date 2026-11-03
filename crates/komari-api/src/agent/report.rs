//! `POST /api/clients/report` — one-shot HTTP metric report.

use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use komari_auth::Role;
use serde_json::Value;

use super::unauthorized;
use crate::report::Report;
use crate::response::{error_json, status_success};
use crate::state::ApiState;

pub async fn upload_report(State(state): State<ApiState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, usize::MAX).await {
        Ok(body) => body,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, error_json("Invalid request body")).into_response();
        }
    };

    let context = komari_auth::authenticate_request_with(
        &state.auth,
        &parts.headers,
        &parts.uri,
        &parts.method,
        Some(&body),
    )
    .await;
    if !context.permits(&[Role::Admin, Role::Client]) {
        return unauthorized();
    }

    let value = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, error_json("Invalid request body")).into_response();
        }
    };

    let mut report = match serde_json::from_value::<Report>(value) {
        Ok(report) => report,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, error_json("Invalid request body")).into_response();
        }
    };

    // Go prefers the body UUID and falls back to the authenticated agent UUID.
    let uuid = if report.uuid.is_empty() {
        context.client_uuid.clone().unwrap_or_default()
    } else {
        report.uuid.clone()
    };
    if uuid.is_empty() {
        return (StatusCode::BAD_REQUEST, error_json("UUID is required")).into_response();
    }
    report.uuid = uuid.clone();
    report.mark_updated();

    state.report_cache.record(&uuid, report.clone());
    state.hub.set_latest_report(&uuid, report);
    state.presence.refresh(&uuid);

    (StatusCode::OK, status_success()).into_response()
}
