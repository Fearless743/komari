//! Login sessions (`/api/admin/session/**`).

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use komari_auth::RequireAdmin;
use serde_json::{Value, json};

use super::{audit, bad_request, cookie, internal, ok, parse_value};
use crate::state::ApiState;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/admin/session/get", get(get_sessions))
        .route("/api/admin/session/remove", post(delete_session))
        .route("/api/admin/session/remove/all", post(delete_all_sessions))
}

/// `GET /api/admin/session/get` — Go `admin.GetSessions`.
pub(crate) async fn get_sessions(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
    headers: HeaderMap,
) -> Response {
    match state.db.sessions().list() {
        Ok(sessions) => {
            let current = cookie(&headers, komari_auth::SESSION_COOKIE_NAME).unwrap_or_default();
            axum::Json(json!({
                "status": "success",
                "current": current,
                "data": sessions,
            }))
            .into_response()
        }
        Err(error) => internal(format!("Failed to retrieve sessions: {error}")),
    }
}

/// `POST /api/admin/session/remove` — Go `admin.DeleteSession`.
pub(crate) async fn delete_session(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let value = match parse_value(&body, "Invalid request: ") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Some(session) = value.get("session").and_then(Value::as_str) else {
        return bad_request("Invalid request: missing session");
    };
    match state.db.sessions().delete(session) {
        Ok(_) => {
            audit(&state, &admin, "delete session", "info");
            ok()
        }
        Err(error) => internal(format!("Failed to delete session: {error}")),
    }
}

/// `POST /api/admin/session/remove/all` — Go `admin.DeleteAllSession`.
pub(crate) async fn delete_all_sessions(
    State(state): State<ApiState>,
    admin: RequireAdmin,
) -> Response {
    match state.db.sessions().delete_all() {
        Ok(_) => {
            audit(&state, &admin, "delete all sessions", "warn");
            ok()
        }
        Err(error) => internal(format!("Failed to delete all sessions: {error}")),
    }
}
