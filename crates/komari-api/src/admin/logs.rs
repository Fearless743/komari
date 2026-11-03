//! Audit logs (`/api/admin/logs`).

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use komari_auth::RequireAdmin;
use serde_json::json;

use super::{bad_request, internal};
use crate::response;
use crate::state::ApiState;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new().route("/api/admin/logs", get(get_logs))
}

/// `GET /api/admin/logs?limit=100&page=1` — Go `log.GetLogs`.
pub(crate) async fn get_logs(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query.get("limit").map(String::as_str).unwrap_or("100");
    let page = query.get("page").map(String::as_str).unwrap_or("1");

    let Ok(limit) = limit.parse::<i64>() else {
        return bad_request(format!("Invalid limit: {limit}"));
    };
    if limit <= 0 {
        return bad_request(format!("Invalid limit: {limit}"));
    }
    let Ok(page) = page.parse::<i64>() else {
        return bad_request(format!("Invalid page: {page}"));
    };
    if page <= 0 {
        return bad_request(format!("Invalid page: {page}"));
    }

    let offset = (page - 1).saturating_mul(limit);
    let total = match state.db.logs().count() {
        Ok(total) => total,
        Err(error) => return internal(format!("Failed to count logs: {error}")),
    };
    match state.db.logs().list_page(limit, offset) {
        Ok(logs) => response::success(json!({"logs": logs, "total": total})).into_response(),
        Err(error) => internal(format!("Failed to retrieve logs: {error}")),
    }
}
