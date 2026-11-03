//! Notification rules (`/api/admin/notification/**`).

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use komari_auth::RequireAdmin;
use komari_db::models::{LoadNotification, OfflineNotification};
use serde_json::{Value, json};

use super::{audit, bad_request, internal, ok, parse_json, parse_value};
use crate::response;
use crate::state::ApiState;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/admin/notification/offline", get(list_offline))
        .route("/api/admin/notification/offline/edit", post(edit_offline))
        .route(
            "/api/admin/notification/offline/enable",
            post(enable_offline),
        )
        .route(
            "/api/admin/notification/offline/disable",
            post(disable_offline),
        )
        .route("/api/admin/notification/load", get(list_load))
        .route("/api/admin/notification/load/", get(list_load))
        .route("/api/admin/notification/load/add", post(add_load))
        .route("/api/admin/notification/load/delete", post(delete_load))
        .route("/api/admin/notification/load/edit", post(edit_load))
}

/// `GET /api/admin/notification/offline`.
pub(crate) async fn list_offline(State(state): State<ApiState>, _admin: RequireAdmin) -> Response {
    match state.db.notifications().offline_list() {
        Ok(notifications) => response::success(json!(notifications)).into_response(),
        Err(error) => internal(format!("Failed to list offline notifications: {error}")),
    }
}

/// `POST /api/admin/notification/offline/edit`.
pub(crate) async fn edit_offline(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let notifications: Vec<OfflineNotification> = match parse_json(&body, "Invalid request body: ")
    {
        Ok(notifications) => notifications,
        Err(response) => return response,
    };
    if notifications.is_empty() {
        return bad_request("At least one notification is required");
    }
    for notification in &notifications {
        if notification.client.is_empty() {
            return bad_request("Client UUID cannot be empty");
        }
        if notification.grace_period <= 0 {
            return bad_request("GracePeriod must be a positive integer");
        }
    }
    for notification in &notifications {
        if let Err(error) = state.db.notifications().offline_save(notification) {
            return internal(format!("Failed to edit offline notifications: {error}"));
        }
    }
    audit(&state, &admin, "edit offline notifications", "info");
    ok()
}

/// `POST /api/admin/notification/offline/enable`.
pub(crate) async fn enable_offline(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    set_offline_enabled(&state, &admin, &body, true)
}

/// `POST /api/admin/notification/offline/disable`.
pub(crate) async fn disable_offline(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    set_offline_enabled(&state, &admin, &body, false)
}

fn set_offline_enabled(
    state: &ApiState,
    admin: &RequireAdmin,
    body: &Bytes,
    enabled: bool,
) -> Response {
    let value = match parse_value(body, "Invalid request body: ") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Some(uuids) = value.as_array() else {
        return bad_request("Invalid request body: expected an array");
    };
    for uuid in uuids.iter().filter_map(Value::as_str) {
        let mut notification = state
            .db
            .notifications()
            .offline_get(uuid)
            .ok()
            .flatten()
            .unwrap_or_else(|| OfflineNotification::new(uuid));
        notification.enable = enabled;
        if let Err(error) = state.db.notifications().offline_save(&notification) {
            let action = if enabled { "enable" } else { "disable" };
            return internal(format!("Failed to {action} offline notifications: {error}"));
        }
    }
    let action = if enabled { "enable" } else { "disable" };
    audit(
        state,
        admin,
        &format!("{action} offline notifications"),
        "info",
    );
    ok()
}

/// `GET /api/admin/notification/load`.
pub(crate) async fn list_load(State(state): State<ApiState>, _admin: RequireAdmin) -> Response {
    match state.db.notifications().load_list() {
        Ok(notifications) => response::success(json!(notifications)).into_response(),
        Err(error) => internal(error.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct AddLoadBody {
    #[serde(default)]
    clients: Vec<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    metric: String,
    #[serde(default)]
    threshold: f32,
    #[serde(default)]
    ratio: f32,
    #[serde(default)]
    interval: i64,
}

/// `POST /api/admin/notification/load/add`.
pub(crate) async fn add_load(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let request: AddLoadBody = match parse_json(&body, "") {
        Ok(request) => request,
        Err(response) => return response,
    };
    if request.clients.is_empty() || request.metric.is_empty() || request.threshold == 0.0 {
        return bad_request("missing required fields");
    }
    if request.interval > 4 * 60 || request.interval <= 0 {
        return bad_request("Interval must be between 1 and 240 minutes");
    }
    if request.ratio <= 0.0 || request.ratio > 1.0 {
        return bad_request("Ratio must be between 0 and 1");
    }

    let mut notification = LoadNotification::new(request.name, request.metric);
    notification.clients = request.clients.into();
    notification.threshold = request.threshold;
    notification.ratio = request.ratio;
    notification.interval = request.interval;
    match state.db.notifications().load_insert(&notification) {
        Ok(id) => {
            audit(
                &state,
                &admin,
                &format!("add load notification:{id}"),
                "info",
            );
            response::success(json!({"task_id": id})).into_response()
        }
        Err(error) => internal(error.to_string()),
    }
}

/// `POST /api/admin/notification/load/delete`.
pub(crate) async fn delete_load(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let value = match parse_value(&body, "") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Some(ids) = value.get("id").and_then(Value::as_array) else {
        return bad_request("missing id");
    };
    let ids = ids.iter().filter_map(Value::as_i64).collect::<Vec<_>>();
    match state.db.notifications().load_delete(&ids) {
        Ok(_) => {
            audit(&state, &admin, "delete load notifications", "warn");
            ok()
        }
        Err(error) => internal(error.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct EditLoadBody {
    #[serde(default)]
    notifications: Vec<LoadNotification>,
}

/// `POST /api/admin/notification/load/edit`.
pub(crate) async fn edit_load(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let request: EditLoadBody = match parse_json(&body, "Invalid request data") {
        Ok(request) => request,
        Err(response) => return response,
    };
    for notification in &request.notifications {
        let mut notification = notification.clone();
        if let Err(error) = state.db.notifications().load_save(&mut notification) {
            return internal(error.to_string());
        }
    }
    audit(&state, &admin, "edit load notifications", "info");
    ok()
}
