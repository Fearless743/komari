//! Settings read/write (`/api/admin/settings`).

use std::collections::HashMap;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use komari_auth::RequireAdmin;
use serde_json::Value;

use super::{audit, bad_request, internal, parse_value};
use crate::response;
use crate::state::ApiState;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/admin/settings", get(get_settings).post(edit_settings))
        .route(
            "/api/admin/settings/",
            get(get_settings).post(edit_settings),
        )
}

/// `GET /api/admin/settings` — Go `admin.GetSettings`.
pub(crate) async fn get_settings(State(state): State<ApiState>, _admin: RequireAdmin) -> Response {
    match state.db.config().get_all() {
        Ok(values) => {
            response::success(Value::Object(values.into_iter().collect())).into_response()
        }
        Err(error) => internal(format!("Failed to get settings: {error}")),
    }
}

/// `POST /api/admin/settings` — Go `admin.EditSettings`.
pub(crate) async fn edit_settings(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let value = match parse_value(&body, "Invalid or missing request body: ") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Value::Object(object) = value else {
        return bad_request("Invalid or missing request body: expected a JSON object");
    };

    let values: HashMap<String, Value> = object.into_iter().collect();
    if let Err(error) = state.db.config().set_many(&values) {
        return internal(format!("Failed to update settings: {error}"));
    }

    let mut message = String::from("update settings: ");
    for key in values.keys() {
        message.push_str(key);
        message.push_str(", ");
    }
    if message.len() > 2 {
        message.truncate(message.len() - 2);
    }
    audit(&state, &admin, &message, "info");
    response::success(Value::Null).into_response()
}
