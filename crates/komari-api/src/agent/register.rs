//! `POST /api/clients/register` — auto-discovery registration.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use komari_db::util;

use crate::response::{error as error_response, success};
use crate::state::ApiState;

const AUTO_PREFIX: &str = "Auto-";

/// Mirrors `api/client/autoDiscovery.go`: the `Authorization` header must be
/// exactly `Bearer <auto_discovery_key>` and the stored key must be at least
/// 12 characters long.
pub async fn register_client(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let authorization = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    let key = match state.auto_discovery_key() {
        Ok(key) => key,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_response(format!("Failed to get AutoDiscovery Key: {error}")),
            )
                .into_response();
        }
    };
    if authorization.is_empty()
        || key.is_empty()
        || key.len() < 12
        || authorization != format!("Bearer {key}")
    {
        return (
            StatusCode::FORBIDDEN,
            error_response("Invalid AutoDiscovery Key"),
        )
            .into_response();
    }

    let requested = query.get("name").map(String::as_str).unwrap_or_default();
    let name = if requested.is_empty() {
        format!("{AUTO_PREFIX}{}", util::generate_random_string(8))
    } else {
        format!("{AUTO_PREFIX}{requested}")
    };

    match state.db.clients().create_named(name) {
        Ok(client) => (
            StatusCode::OK,
            success(serde_json::json!({"uuid": client.uuid, "token": client.token})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_response(format!("Failed to create client: {error}")),
        )
            .into_response(),
    }
}
