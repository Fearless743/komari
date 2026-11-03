//! Clipboard entries (`/api/admin/clipboard/**`).

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use komari_auth::RequireAdmin;
use komari_db::models::Clipboard;
use komari_db::repositories::ClipboardPatch;
use serde_json::{Value, json};

use super::{audit, bad_request, internal, ok, parse_json, parse_value};
use crate::response;
use crate::state::ApiState;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/admin/clipboard", get(list).post(create))
        .route("/api/admin/clipboard/remove", post(batch_delete))
        .route("/api/admin/clipboard/{id}", get(get_one).post(update))
        .route("/api/admin/clipboard/{id}/remove", post(delete))
}

#[allow(clippy::result_large_err)] // `Response` is the handler's natural error type.
fn parse_id(id: &str) -> Result<i64, Response> {
    id.parse::<i64>().map_err(|_| bad_request("Invalid ID"))
}

/// `GET /api/admin/clipboard/{id}`.
pub(crate) async fn get_one(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match state.db.clipboard().get(id) {
        Ok(Some(entry)) => response::success(json!(entry)).into_response(),
        Ok(None) => internal("Failed to get clipboard: record not found"),
        Err(error) => internal(format!("Failed to get clipboard: {error}")),
    }
}

/// `GET /api/admin/clipboard`.
pub(crate) async fn list(State(state): State<ApiState>, _admin: RequireAdmin) -> Response {
    match state.db.clipboard().list() {
        Ok(entries) => response::success(json!(entries)).into_response(),
        Err(error) => internal(format!("Failed to list clipboard: {error}")),
    }
}

/// `POST /api/admin/clipboard`.
pub(crate) async fn create(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let mut entry: Clipboard = match parse_json(&body, "Invalid request: ") {
        Ok(entry) => entry,
        Err(response) => return response,
    };
    match state.db.clipboard().insert(&entry) {
        Ok(id) => {
            entry.id = id;
            audit(&state, &admin, &format!("create clipboard:{id}"), "info");
            response::success(json!(entry)).into_response()
        }
        Err(error) => internal(format!("Failed to create clipboard: {error}")),
    }
}

/// `POST /api/admin/clipboard/{id}`.
pub(crate) async fn update(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let value = match parse_value(&body, "Invalid request: ") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Some(object) = value.as_object() else {
        return bad_request("Invalid request: expected an object");
    };
    let mut patch = ClipboardPatch::default();
    for (key, value) in object {
        match key.as_str() {
            "text" => patch.text = value.as_str().map(str::to_owned),
            "name" => patch.name = value.as_str().map(str::to_owned),
            "weight" => patch.weight = value.as_i64(),
            "remark" => patch.remark = value.as_str().map(str::to_owned),
            _ => {}
        }
    }
    match state.db.clipboard().patch(id, &patch) {
        Ok(_) => {
            audit(&state, &admin, &format!("update clipboard:{id}"), "info");
            ok()
        }
        Err(error) => internal(format!("Failed to update clipboard: {error}")),
    }
}

/// `POST /api/admin/clipboard/{id}/remove`.
pub(crate) async fn delete(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match state.db.clipboard().delete(id) {
        Ok(true) => {
            audit(&state, &admin, &format!("delete clipboard:{id}"), "warn");
            ok()
        }
        Ok(false) => internal("Failed to delete clipboard: record not found"),
        Err(error) => internal(format!("Failed to delete clipboard: {error}")),
    }
}

/// `POST /api/admin/clipboard/remove`.
pub(crate) async fn batch_delete(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let value = match parse_value(&body, "Invalid request: ") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Some(ids) = value.get("ids").and_then(Value::as_array) else {
        return bad_request("Invalid request: missing ids");
    };
    if ids.is_empty() {
        return bad_request("IDs cannot be empty");
    }
    let ids = ids.iter().filter_map(Value::as_i64).collect::<Vec<_>>();
    match state.db.clipboard().delete_batch(&ids) {
        Ok(_) => {
            audit(
                &state,
                &admin,
                &format!("batch delete clipboard: {} items", ids.len()),
                "warn",
            );
            ok()
        }
        Err(error) => internal(format!("Failed to batch delete clipboard: {error}")),
    }
}
