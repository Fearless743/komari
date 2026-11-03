//! JSON response helpers matching the Go `api.Response` envelope.

use axum::Json;
use serde_json::{Value, json};

/// `{"status":"success","message":"","data":...}` — Go `RespondSuccess`.
pub fn success(data: Value) -> Json<Value> {
    Json(json!({
        "status": "success",
        "message": "",
        "data": data,
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
        None => Json(json!({
            "status": "success",
            "message": "",
        })),
    }
}

/// `{"status":"error","message":"..."}` — Go `RespondError` (nil `data` is
/// omitted by `omitempty`).
pub fn error(message: impl Into<String>) -> Json<Value> {
    Json(json!({
        "status": "error",
        "message": message.into(),
    }))
}

/// `{"error":"..."}` — used by the raw report handlers.
pub fn error_json(message: impl Into<String>) -> Json<Value> {
    Json(json!({ "error": message.into() }))
}

/// `{"status":"success"}` — used by report and basic-info handlers.
pub fn status_success() -> Json<Value> {
    Json(json!({ "status": "success" }))
}
