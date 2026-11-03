//! `/api/rpc2`: JSON-RPC 2.0 over HTTP POST and WebSocket.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::Method;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use serde_json::json;

use super::context::{RpcContext, detect};
use super::protocol::{INVALID_REQUEST, RpcError, RpcRequest, RpcResponse, parse_requests};
use super::registry::dispatch;
use crate::state::ApiState;
use crate::ws::check_origin;

/// Routes for `/api/rpc2` (GET WebSocket, POST JSON-RPC).
pub fn rpc_router() -> Router<ApiState> {
    Router::new().route("/api/rpc2", any(rpc_entry))
}

async fn rpc_entry(State(state): State<ApiState>, request: Request) -> Response {
    let method = request.method().clone();
    match method {
        Method::GET => rpc_ws(State(state), request).await,
        Method::POST => rpc_post(State(state), request).await,
        _ => (
            StatusCode::METHOD_NOT_ALLOWED,
            axum::Json(json!({"error": "method not allowed"})),
        )
            .into_response(),
    }
}

/// Standalone router with state applied.
pub fn rpc_router_with_state(state: ApiState) -> Router {
    rpc_router().with_state(state)
}

async fn rpc_context(
    state: &ApiState,
    headers: &axum::http::HeaderMap,
    uri: &axum::http::Uri,
) -> RpcContext {
    detect(
        state.db.clone(),
        state.hub.clone(),
        state.report_cache.clone(),
        headers,
        uri,
    )
    .await
}

/// `GET /api/rpc2` — WebSocket transport.
async fn rpc_ws(State(state): State<ApiState>, request: Request) -> Response {
    let (mut parts, _body) = request.into_parts();
    if !state.allow_cors() && !check_origin(&parts.headers, state.ws_disable_origin()) {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .header("x-content-type-options", "nosniff")
            .body(Body::from("Forbidden\n"))
            .expect("static 403 response is valid");
    }

    let context = rpc_context(&state, &parts.headers, &parts.uri).await;
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(upgrade) => upgrade,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({
                    "status": "error",
                    "error": format!("Failed to upgrade to WebSocket.{error}"),
                })),
            )
                .into_response();
        }
    };
    upgrade.on_upgrade(move |socket| handle_socket(socket, context))
}

async fn handle_socket(mut socket: WebSocket, context: RpcContext) {
    while let Some(message) = socket.recv().await {
        let text = match message {
            Ok(Message::Text(text)) => text,
            Ok(Message::Binary(bytes)) => match String::from_utf8(bytes.to_vec()) {
                Ok(text) => text.into(),
                Err(_) => {
                    if send_error(
                        &mut socket,
                        None,
                        INVALID_REQUEST,
                        "bad request: invalid utf-8",
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                    continue;
                }
            },
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            Err(_) => break,
        };

        let request: RpcRequest = match serde_json::from_str(&text) {
            Ok(request) => request,
            Err(error) => {
                let message = format!("bad request: {error}");
                if send_error(&mut socket, None, INVALID_REQUEST, &message)
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
        };

        if let Some(error) = request.validate() {
            if send_response(&mut socket, error.response(request.id.clone()))
                .await
                .is_err()
            {
                break;
            }
            continue;
        }

        let response = dispatch(&context, &request);
        if send_response(&mut socket, response).await.is_err() {
            break;
        }
    }
}

async fn send_error(
    socket: &mut WebSocket,
    id: Option<serde_json::Value>,
    code: i64,
    message: &str,
) -> Result<(), axum::Error> {
    send_response(
        socket,
        RpcResponse::error(id, RpcError::new(code, message, None)),
    )
    .await
}

async fn send_response(socket: &mut WebSocket, response: RpcResponse) -> Result<(), axum::Error> {
    let payload = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_owned());
    socket.send(Message::Text(payload.into())).await
}

/// `POST /api/rpc2` — JSON-RPC over HTTP (single or batch).
async fn rpc_post(State(state): State<ApiState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let context = rpc_context(&state, &parts.headers, &parts.uri).await;
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(
                    RpcError::new(
                        super::protocol::PARSE_ERROR,
                        "read body error",
                        Some(json!(error.to_string())),
                    )
                    .response(None),
                ),
            )
                .into_response();
        }
    };

    let requests = match parse_requests(&bytes) {
        Ok(requests) => requests,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, axum::Json(error.response(None))).into_response();
        }
    };

    let responses = requests
        .iter()
        .map(|request| dispatch(&context, request))
        .collect::<Vec<_>>();
    if responses.len() == 1 {
        let response = responses.into_iter().next();
        axum::Json(response).into_response()
    } else {
        axum::Json(responses).into_response()
    }
}
