//! Admin test/update endpoints (`/api/admin/test/**`, `/api/admin/update/**`).

use std::collections::HashMap;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::http::header::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use komari_auth::RequireAdmin;
use komari_db::LocalTime;
use komari_db::models::EventMessage;
use komari_notify::NotificationService;
use serde_json::json;

use super::{audit, bad_request, internal, ok, parse_json};
use crate::response;
use crate::state::ApiState;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/admin/test/geoip", get(test_geoip))
        .route("/api/admin/test/sendMessage", post(test_send_message))
        .route("/api/admin/update/mmdb", post(update_mmdb))
        .route("/api/admin/update/user", post(update_user))
        .route(
            "/api/admin/update/favicon",
            axum::routing::put(upload_favicon).post(delete_favicon),
        )
}

/// `GET /api/admin/test/geoip?ip=...` — Go `test.TestGeoIp`.
pub(crate) async fn test_geoip(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let enabled = state
        .db
        .config()
        .get_as::<bool>(komari_config::keys::GEO_IP_ENABLED_KEY)
        .ok()
        .flatten()
        .unwrap_or(false);
    if !enabled {
        return bad_request("GeoIP is not enabled in the configuration.");
    }

    let ip = query
        .get("ip")
        .filter(|ip| !ip.is_empty())
        .cloned()
        .or_else(|| header_value(&headers, "CF-Connecting-IP"))
        .or_else(|| {
            header_value(&headers, "X-Forwarded-For")
                .and_then(|value| value.split(',').next().map(str::trim).map(str::to_owned))
        })
        .or_else(|| header_value(&headers, "X-Real-IP"))
        .unwrap_or_else(|| "127.0.0.1".to_owned());

    let Some(service) = state.geoip.as_ref() else {
        return internal("Failed to get GeoIP record: service is not initialized");
    };
    match service.try_lookup_str(&ip).await {
        Ok(info) => response::success(json!(info)).into_response(),
        Err(error) => internal(format!("Failed to get GeoIP record: {error}")),
    }
}

/// `POST /api/admin/test/sendMessage` — Go `test.TestSendMessage`.
pub(crate) async fn test_send_message(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
) -> Response {
    let service = match NotificationService::from_db(&state.db).await {
        Ok(service) => service,
        Err(error) => return internal(format!("Failed to send message: {error}")),
    };
    let event = EventMessage {
        event: "Test".to_owned(),
        message: "This is a test message from Komari.".to_owned(),
        time: LocalTime::now(),
        ..EventMessage::default()
    };
    match service.send_event(&event).await {
        Ok(_) => ok(),
        Err(error) => internal(format!("Failed to send message: {error}")),
    }
}

/// `POST /api/admin/update/mmdb` — Go `update.UpdateMmdbGeoIP`.
pub(crate) async fn update_mmdb(State(state): State<ApiState>, admin: RequireAdmin) -> Response {
    let Some(service) = state.geoip.as_ref() else {
        return internal("Failed to update GeoIP database service is not initialized");
    };
    match service.update_database().await {
        Ok(()) => {
            audit(&state, &admin, "GeoIP database updated", "info");
            ok()
        }
        Err(error) => internal(format!("Failed to update GeoIP database {error}")),
    }
}

#[derive(serde::Deserialize)]
struct UpdateUserBody {
    #[serde(default)]
    uuid: String,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    sso_type: Option<String>,
}

/// `POST /api/admin/update/user` — Go `update.UpdateUser`.
pub(crate) async fn update_user(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let request: UpdateUserBody = match parse_json(&body, "Invalid or missing request body: ") {
        Ok(request) => request,
        Err(response) => return response,
    };
    if request.uuid.is_empty() {
        return bad_request("Invalid or missing request body: missing uuid");
    }
    if request.password.is_none() && request.username.is_none() {
        return bad_request("At least one field (username or password) must be provided");
    }
    if let Some(username) = request.username.as_deref() {
        if username.len() < 3 {
            return bad_request("Username must be at least 3 characters long");
        }
    }
    if let Some(password) = request.password.as_deref() {
        if password.len() < 6 {
            return bad_request("Password must be at least 6 characters long");
        }
    }

    if let Err(error) = state.db.accounts().update_profile(
        &request.uuid,
        request.username.as_deref(),
        request.sso_type.as_deref(),
    ) {
        return internal(format!("Failed to update user: {error}"));
    }
    if let Some(password) = request.password.as_deref() {
        if let Err(error) = state.db.accounts().set_password(&request.uuid, password) {
            return internal(format!("Failed to update user: {error}"));
        }
    }
    audit(&state, &admin, "User updated", "warn");
    response::success(json!({"uuid": request.uuid})).into_response()
}

/// `PUT /api/admin/update/favicon` — Go `update.UploadFavicon`.
pub(crate) async fn upload_favicon(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    const MAX_SIZE: usize = 5 * 1024 * 1024;
    if body.len() > MAX_SIZE {
        return response::error_status(
            StatusCode::PAYLOAD_TOO_LARGE,
            "File too large. Maximum size is 5MB",
        );
    }
    let data_dir = state.data_dir();
    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        return internal(format!("Failed to save favicon: {error}"));
    }
    if let Err(error) = std::fs::write(data_dir.join(komari_web::FAVICON_FILE), &body) {
        return internal(format!("Failed to save favicon: {error}"));
    }
    audit(&state, &admin, "Favicon uploaded", "info");
    ok()
}

/// `POST /api/admin/update/favicon` — Go `update.DeleteFavicon`.
pub(crate) async fn delete_favicon(State(state): State<ApiState>, admin: RequireAdmin) -> Response {
    let path = state.data_dir().join(komari_web::FAVICON_FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            audit(&state, &admin, "Favicon deleted", "info");
            ok()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            response::error_status(StatusCode::NOT_FOUND, "Favicon not found")
        }
        Err(error) => internal(format!("Failed to delete favicon: {error}")),
    }
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
