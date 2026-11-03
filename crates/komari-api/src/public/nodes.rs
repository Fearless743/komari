//! `GET /api/nodes` — public node list.
//!
//! Port of Go `api/public.GetNodesInformation`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use komari_auth::AuthContext;
use serde_json::Value;

use crate::state::ApiState;

use super::respond_success;

pub(crate) async fn get_nodes(State(state): State<ApiState>, context: AuthContext) -> Response {
    let clients = match state.db.clients().list() {
        Ok(clients) => clients,
        Err(error) => {
            return super::respond_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to retrieve client information: {error}"),
            );
        }
    };

    // Go only treats a *session* as logged in here; an API key is not enough
    // for hidden clients to become visible.
    let is_login = context.session_token.is_some();

    let mut visible = Vec::with_capacity(clients.len());
    for mut client in clients {
        if client.hidden && !is_login {
            continue;
        }
        // Private fields are cleared for every caller, including admins,
        // matching Go's loop in `GetNodesInformation`.
        client.ipv4.clear();
        client.ipv6.clear();
        client.remark.clear();
        client.version.clear();
        client.token.clear();
        visible.push(serde_json::to_value(&client).unwrap_or(Value::Null));
    }

    respond_success(Value::Array(visible))
}
