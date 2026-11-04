//! Agent route handlers.

pub mod basic_info;
pub mod report;

use axum::response::Response;

/// Go-compatible `401 {"status":"error","message":"Unauthorized."}`.
pub(crate) fn unauthorized() -> Response {
    komari_auth::unauthorized_response()
}
