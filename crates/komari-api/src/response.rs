//! JSON response helpers matching the Go `api.Response` envelope.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

/// `{"status":"success","message":"","data":...}` — Go `RespondSuccess`.
pub fn success(data: Value) -> Json<Value> {
    Json(json!({
        "status": "success",
        "message": "",
        "data": data,
    }))
}

/// Go `RespondSuccess(nil)`: the `omitempty` data field is omitted.
pub fn success_no_data() -> Json<Value> {
    Json(json!({
        "status": "success",
        "message": "",
    }))
}

/// Like [`success`], but omits `data` entirely when it is `None`.
///
/// Go's `Response.Data` uses `json:"data,omitempty"`, so handlers that pass a
/// nil interface (for example `/api/recent/:uuid` with no buffered report)
/// produce a body without a `data` key.
pub fn success_optional(data: Option<Value>) -> Json<Value> {
    match data {
        Some(data) => success(data),
        None => success_no_data(),
    }
}

/// Go `RespondSuccessMessage`: a success envelope with a custom message.
///
/// `data` follows Go's `json:"data,omitempty"`: a JSON `null` is omitted.
pub fn success_message(message: impl Into<String>, data: Value) -> Json<Value> {
    let message = message.into();
    if data.is_null() {
        return Json(json!({
            "status": "success",
            "message": message,
        }));
    }
    Json(json!({
        "status": "success",
        "message": message,
        "data": data,
    }))
}

/// `{"status":"error","message":"..."}` — Go `RespondError` (nil `data` is
/// omitted by `omitempty`).
pub fn error(message: impl Into<String>) -> Json<Value> {
    Json(json!({
        "status": "error",
        "message": message.into(),
    }))
}

/// Error envelope with an explicit HTTP status code.
pub fn error_status(status: StatusCode, message: impl Into<String>) -> Response {
    (status, error(message)).into_response()
}

/// `{"error":"..."}` — used by the raw report handlers.
pub fn error_json(message: impl Into<String>) -> Json<Value> {
    Json(json!({ "error": message.into() }))
}

/// `{"status":"success"}` — used by report and basic-info handlers.
pub fn status_success() -> Json<Value> {
    Json(json!({ "status": "success" }))
}
