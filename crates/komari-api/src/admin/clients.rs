//! Client management (`/api/admin/client/**`, `/api/admin/record/**`).

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use komari_auth::RequireAdmin;
use komari_db::{ClientPatch, LocalTime};
use serde_json::{Value, json};

use super::{audit, bad_request, internal, ok, parse_value};
use crate::response;
use crate::state::ApiState;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/admin/client/add", post(add_client))
        .route("/api/admin/client/list", get(list_clients))
        .route("/api/admin/client/order", post(order_weight))
        .route("/api/admin/client/{uuid}", get(get_client))
        .route("/api/admin/client/{uuid}/edit", post(edit_client))
        .route("/api/admin/client/{uuid}/remove", post(remove_client))
        .route("/api/admin/client/{uuid}/token", get(get_client_token))
        .route("/api/admin/record/clear", post(clear_record))
        .route("/api/admin/record/clear/all", post(clear_record))
}

/// `POST /api/admin/client/add` — Go `admin.AddClient`.
pub(crate) async fn add_client(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    // Go treats a missing/invalid body exactly like `{"name": ""}`.
    let name = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|value| value.get("name").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();

    let client = match state.db.clients().create_named(name.clone()) {
        Ok(client) => client,
        Err(error) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({"status": "error", "message": error.to_string()})),
            )
                .into_response();
        }
    };

    if name.is_empty() {
        return axum::Json(json!({
            "status": "success",
            "uuid": client.uuid,
            "token": client.token,
        }))
        .into_response();
    }

    audit(
        &state,
        &admin,
        &format!("create client:{}", client.uuid),
        "info",
    );
    axum::Json(json!({
        "status": "success",
        "uuid": client.uuid,
        "token": client.token,
        "message": "",
    }))
    .into_response()
}

/// `GET /api/admin/client/list` — Go `admin.ListClients` (raw array).
pub(crate) async fn list_clients(State(state): State<ApiState>, _admin: RequireAdmin) -> Response {
    match state.db.clients().list() {
        Ok(clients) => axum::Json(clients).into_response(),
        Err(error) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({"status": "error", "message": error.to_string()})),
        )
            .into_response(),
    }
}

/// `GET /api/admin/client/{uuid}` — Go `admin.GetClient` (raw object).
pub(crate) async fn get_client(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
    Path(uuid): Path<String>,
) -> Response {
    if uuid.is_empty() {
        return bad_request("Invalid or missing UUID");
    }
    match state.db.clients().get(&uuid) {
        Ok(Some(client)) => axum::Json(client).into_response(),
        Ok(None) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({"status": "error", "message": "record not found"})),
        )
            .into_response(),
        Err(error) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({"status": "error", "message": error.to_string()})),
        )
            .into_response(),
    }
}

/// `POST /api/admin/client/{uuid}/edit` — Go `admin.EditClient`.
pub(crate) async fn edit_client(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    Path(uuid): Path<String>,
    body: Bytes,
) -> Response {
    if uuid.is_empty() {
        return bad_request("Invalid or missing UUID");
    }
    let value = match parse_value(&body, "") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let patch = match client_patch(&value) {
        Ok(patch) => patch,
        Err(message) => return bad_request(message),
    };
    match state.db.clients().patch(&uuid, &patch) {
        Ok(_) => {
            audit(&state, &admin, &format!("edit client:{uuid}"), "info");
            if let Ok(Some(client)) = state.db.clients().get(&uuid) {
                let service = state.ddns.clone();
                tokio::spawn(async move {
                    let _ = service.sync_client(&client, "client_edit", false).await;
                });
            }
            ok()
        }
        Err(error) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({"status": "error", "message": error.to_string()})),
        )
            .into_response(),
    }
}

/// `POST /api/admin/client/{uuid}/remove` — Go `admin.RemoveClient`.
pub(crate) async fn remove_client(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    Path(uuid): Path<String>,
) -> Response {
    match state.db.clients().delete(&uuid) {
        Ok(_) => {
            audit(&state, &admin, &format!("delete client:{uuid}"), "warn");
            state.hub.remove_latest_report(&uuid);
            ok()
        }
        Err(error) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({
                "status": "error",
                "error": format!("Failed to delete client{error}"),
            })),
        )
            .into_response(),
    }
}

/// `GET /api/admin/client/{uuid}/token` — Go `admin.GetClientToken`.
pub(crate) async fn get_client_token(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
    Path(uuid): Path<String>,
) -> Response {
    if uuid.is_empty() {
        return bad_request("Invalid or missing UUID");
    }
    match state.db.clients().get(&uuid) {
        Ok(Some(client)) => axum::Json(json!({
            "status": "success",
            "token": client.token,
            // Go has a typo in this key; keep it for frontend compatibility.
            "message:": "",
        }))
        .into_response(),
        Ok(None) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({"status": "error", "message": "record not found"})),
        )
            .into_response(),
        Err(error) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({"status": "error", "message": error.to_string()})),
        )
            .into_response(),
    }
}

/// `POST /api/admin/client/order` — Go `admin.OrderWeight`.
pub(crate) async fn order_weight(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let value = match parse_value(&body, "Invalid or missing request body: ") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Some(object) = value.as_object() else {
        return bad_request("Invalid or missing request body: expected an object");
    };
    for (uuid, weight) in object {
        let Some(weight) = weight.as_i64() else {
            return bad_request(format!("Invalid weight for client {uuid}"));
        };
        let patch = ClientPatch {
            weight: Some(weight),
            ..ClientPatch::default()
        };
        if let Err(error) = state.db.clients().patch(uuid, &patch) {
            return internal(format!("Failed to update client weight: {error}"));
        }
    }
    audit(&state, &admin, "order clients", "info");
    response::success(Value::Null).into_response()
}

/// `POST /api/admin/record/clear` — Go `admin.ClearRecord`.
pub(crate) async fn clear_record(State(state): State<ApiState>, admin: RequireAdmin) -> Response {
    match state.db.records().delete_all() {
        Ok(()) => {
            audit(&state, &admin, "clear records", "warn");
            ok()
        }
        Err(error) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({
                "status": "error",
                "message": format!("Failed to delete Record{error}"),
            })),
        )
            .into_response(),
    }
}

/// Convert the Go `SaveClient` map into a [`ClientPatch`].
///
/// Unknown keys are ignored. Type mismatches produce a 400 instead of a
/// database error.
pub(crate) fn client_patch(value: &Value) -> Result<ClientPatch, String> {
    let Some(object) = value.as_object() else {
        return Err("Invalid request body: expected an object".to_owned());
    };
    let mut patch = ClientPatch::default();
    for (key, value) in object {
        if key == "uuid" {
            continue;
        }
        match key.as_str() {
            "name" => patch.name = Some(string_field(key, value)?),
            "token" => patch.token = Some(string_field(key, value)?),
            "cpu_name" => patch.cpu_name = Some(string_field(key, value)?),
            "virtualization" => patch.virtualization = Some(string_field(key, value)?),
            "arch" => patch.arch = Some(string_field(key, value)?),
            "cpu_cores" => patch.cpu_cores = Some(int_field(key, value)?),
            "os" => patch.os = Some(string_field(key, value)?),
            "kernel_version" => patch.kernel_version = Some(string_field(key, value)?),
            "gpu_name" => patch.gpu_name = Some(string_field(key, value)?),
            "ipv4" => patch.ipv4 = Some(string_field(key, value)?),
            "ipv6" => patch.ipv6 = Some(string_field(key, value)?),
            "region" => patch.region = Some(string_field(key, value)?),
            "remark" => patch.remark = Some(string_field(key, value)?),
            "public_remark" => patch.public_remark = Some(string_field(key, value)?),
            "mem_total" => patch.mem_total = Some(int_field(key, value)?),
            "swap_total" => patch.swap_total = Some(int_field(key, value)?),
            "disk_total" => patch.disk_total = Some(int_field(key, value)?),
            "version" => patch.version = Some(string_field(key, value)?),
            "weight" => patch.weight = Some(int_field(key, value)?),
            "price" => patch.price = Some(float_field(key, value)?),
            "billing_cycle" => patch.billing_cycle = Some(int_field(key, value)?),
            "auto_renewal" => patch.auto_renewal = Some(bool_field(key, value)?),
            "currency" => patch.currency = Some(string_field(key, value)?),
            "expired_at" => {
                let raw = string_field(key, value)?;
                let time = LocalTime::parse(&raw)
                    .map_err(|error| format!("Invalid expired_at: {error}"))?;
                patch.expired_at = Some(time);
            }
            "group" => patch.group = Some(string_field(key, value)?),
            "tags" => patch.tags = Some(string_field(key, value)?),
            "ddns_enabled" => patch.ddns_enabled = Some(bool_field(key, value)?),
            "ddns_hostname" => patch.ddns_hostname = Some(string_field(key, value)?),
            "ddns_record_id" => patch.ddns_record_id = Some(string_field(key, value)?),
            "ddns_record_type" => patch.ddns_record_type = Some(string_field(key, value)?),
            "hidden" => patch.hidden = Some(bool_field(key, value)?),
            "traffic_limit" => patch.traffic_limit = Some(int_field(key, value)?),
            "traffic_limit_type" => patch.traffic_limit_type = Some(string_field(key, value)?),
            _ => {}
        }
    }
    Ok(patch)
}

fn string_field(key: &str, value: &Value) -> Result<String, String> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("Invalid {key}: expected a string"))
}

fn int_field(key: &str, value: &Value) -> Result<i64, String> {
    if let Some(number) = value.as_i64() {
        return Ok(number);
    }
    if let Some(number) = value.as_f64() {
        if number.is_finite() {
            return Ok(number.trunc() as i64);
        }
    }
    Err(format!("Invalid {key}: expected an integer"))
}

fn float_field(key: &str, value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .ok_or_else(|| format!("Invalid {key}: expected a number"))
}

fn bool_field(key: &str, value: &Value) -> Result<bool, String> {
    value
        .as_bool()
        .ok_or_else(|| format!("Invalid {key}: expected a boolean"))
}
