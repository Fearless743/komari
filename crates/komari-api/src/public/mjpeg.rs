//! `GET /api/mjpeg_live` — placeholder for the MJPEG status stream.
//!
//! Go renders a live PNG/JPEG status table from the JSON-RPC node data and an
//! externally downloaded MiSans font (`./data/font.ttf`). Neither the JSON-RPC
//! layer nor the font pipeline is part of this task, so the route answers with
//! an explicit `501` instead of hanging the client on an empty stream. This is
//! registered in `crates/komari-api/README.md`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

pub(crate) async fn mjpeg_live() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "status": "error",
            "message": "MJPEG live stream is not implemented in the Rust backend",
        })),
    )
        .into_response()
}
