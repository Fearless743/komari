//! `POST /api/clients/report` — one-shot HTTP metric report.

use std::collections::HashMap;

use axum::body::to_bytes;
use axum::extract::{Query, Request, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::{auth_failure_response, authenticate, unauthorized};
use crate::report::Report;
use crate::response::{error_json, status_success};
use crate::state::ApiState;

pub async fn upload_report(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let query_token = query.get("token").cloned();
    let token = komari_auth::extract_agent_token(&headers, query_token.as_deref());
    let mut identity = match token.as_deref() {
        Some(token) => match authenticate(&state, token) {
            Ok(identity) => Some(identity),
            Err(failure) => return auth_failure_response(failure),
        },
        None => None,
    };

    let max_body = state.config().max_report_body;
    let body = match to_bytes(request.into_body(), max_body).await {
        Ok(body) => body,
        Err(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                error_json("Invalid request body"),
            )
                .into_response();
        }
    };

    let value = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(_) => {
            // Without a credential the Go identity middleware classifies the
            // request as a guest before the handler parses the body.
            if identity.is_none() {
                return unauthorized();
            }
            return (StatusCode::BAD_REQUEST, error_json("Invalid request body")).into_response();
        }
    };

    if identity.is_none() {
        // IdentityMiddleware also accepts a token inside the JSON body for
        // non-GET requests.
        let Some(body_token) = value.get("token").and_then(Value::as_str) else {
            return unauthorized();
        };
        identity = match authenticate(&state, body_token) {
            Ok(identity) => Some(identity),
            Err(failure) => return auth_failure_response(failure),
        };
    }
    let identity = match identity {
        Some(identity) => identity,
        None => return unauthorized(),
    };

    let mut report = match serde_json::from_value::<Report>(value) {
        Ok(report) => report,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, error_json("Invalid request body")).into_response();
        }
    };

    let uuid = if report.uuid.is_empty() {
        identity.uuid.clone()
    } else {
        report.uuid.clone()
    };
    if uuid.is_empty() {
        return (StatusCode::BAD_REQUEST, error_json("UUID is required")).into_response();
    }
    report.uuid = uuid.clone();
    report.mark_updated();

    if let Err(message) = crate::report::save_report(&state.db, &uuid, &report) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error_json(message)).into_response();
    }

    state
        .hub
        .set_latest_report(&uuid, crate::report::cache_report(report));
    state.presence.refresh(&uuid);

    (StatusCode::OK, status_success()).into_response()
}
