//! Public (browser / unauthenticated) API routes.
//!
//! This module ports the handlers in Go `api/public/*.go` and the route table
//! from `cmd/server.go`. The router is extended by later commits in this
//! series; [`public_router`] always mirrors the Go route table.
//!
//! [`public_router`] returns the routes with `ApiState` state, matching
//! [`crate::client_router`]. [`public_router_with_state`] also installs B03's
//! private-site middleware, which is the Go server's global behaviour.

mod login;
mod nodes;
mod records;
mod settings;
mod version;

use axum::Json;
use axum::Router;
use axum::http::header::{COOKIE, HOST, USER_AGENT};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use komari_auth::PrivateSite;
use serde_json::{Value, json};

use crate::response;
use crate::state::ApiState;

pub use version::{VERSION, VERSION_HASH};

/// Cookie carrying the login session (Go `session_token`).
pub(crate) const SESSION_COOKIE: &str = komari_auth::SESSION_COOKIE_NAME;
/// Cookie carrying the pending OAuth `state` value.
pub(crate) const OAUTH_STATE_COOKIE: &str = "oauth_state";
/// Cookie carrying the account UUID for an OAuth binding flow.
pub(crate) const BINDING_COOKIE: &str = "binding_external_account";

/// Routes owned by the public/browser API.
///
/// The returned router still needs an [`ApiState`]. Compose it with other
/// routers that share the same state, then call `with_state` once:
///
/// ```no_run
/// use axum::Router;
/// use komari_api::{ApiState, client_router, public_router};
///
/// # fn build(state: ApiState) -> Router {
/// Router::new()
///     .merge(public_router())
///     .merge(client_router())
///     .with_state(state)
/// # }
/// ```
pub fn public_router() -> Router<ApiState> {
    Router::new()
        .route("/ping", any(ping))
        .route("/api/nodes", get(nodes::get_nodes))
        .route("/api/public", get(settings::get_public_settings))
        .route("/api/version", get(version::get_version))
        .route("/api/me", get(login::get_me))
        .route("/api/login", post(login::login))
        .route("/api/logout", get(login::logout))
        .route("/api/recent/{uuid}", get(records::get_recent))
        .route("/api/records/load", get(records::get_records))
}

/// Public routes with state, including the private-site guard.
///
/// Go installs `PrivateSiteMiddleware` globally; this helper does the same for
/// the public route group. Server assemblies that apply `PrivateSite` to the
/// whole application can use [`public_router`] instead.
pub fn public_router_with_state(state: ApiState) -> Router {
    let auth = state.auth.clone();
    public_router()
        .layer(PrivateSite::new(auth))
        .with_state(state)
}

/// Load the configured OAuth provider and install it on `state`.
///
/// Port of Go's `oauth.Initialize` call in `cmd/server.go`: seeds default
/// provider rows, then loads `o_auth_provider` (falling back to GitHub).
pub async fn initialize_oauth(state: &ApiState) -> komari_oauth::Result<()> {
    let manager = komari_oauth::OAuthManager::new();
    manager.initialize_from_db(&state.db).await?;
    state.set_oauth_provider(manager.current());
    Ok(())
}

async fn ping() -> &'static str {
    "pong"
}

/// Go `api.RespondSuccess`: `{"status":"success","message":"","data":...}`.
pub(crate) fn respond_success(data: Value) -> Response {
    (StatusCode::OK, response::success(data)).into_response()
}

/// Go `api.RespondError`: `{"status":"error","message":"..."}`.
pub(crate) fn respond_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, response::error(message)).into_response()
}

/// OAuth handlers use a raw `{"status":"error","error":"..."}` body in Go.
pub(crate) fn respond_raw_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "status": "error",
            "error": message.into(),
        })),
    )
        .into_response()
}

/// Build a `302 Found` response with the supplied `Location`.
pub(crate) fn redirect(location: &str) -> Response {
    let mut response = StatusCode::FOUND.into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}

/// Append a Go-compatible `Set-Cookie` header.
///
/// Gin uses `http.SetCookie` with `Secure=false` and `HttpOnly=true`. A
/// positive `max_age` emits `Max-Age=N`, a negative one emits `Max-Age=0`
/// (deletion) and zero emits a session cookie without `Max-Age`.
pub(crate) fn set_cookie(response: &mut Response, name: &str, value: &str, max_age: i64) {
    let mut cookie = format!("{name}={value}; Path=/");
    if max_age > 0 {
        cookie.push_str(&format!("; Max-Age={max_age}"));
    } else if max_age < 0 {
        cookie.push_str("; Max-Age=0");
    }
    cookie.push_str("; HttpOnly");
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

/// Read the first cookie with the supplied name.
pub(crate) fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name).then(|| value.to_owned())
        })
        .next()
}

/// Client IP with the same header precedence as `komari-auth`.
pub(crate) fn client_ip(headers: &HeaderMap) -> String {
    if let Some(value) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
    {
        let value = value.trim();
        if !value.is_empty() {
            return value.to_owned();
        }
    }
    headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

/// User agent used when creating a session.
pub(crate) fn user_agent(headers: &HeaderMap) -> String {
    headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// Write an audit-log row, ignoring database failures like Go's `auditlog.Log`.
pub(crate) fn audit(
    state: &ApiState,
    headers: &HeaderMap,
    uuid: &str,
    message: impl Into<String>,
    msg_type: &str,
) {
    let ip = client_ip(headers);
    if let Err(error) = state.db.logs().log(&ip, uuid, message, msg_type) {
        tracing::warn!(%error, "failed to write audit log");
    }
}

/// Go `utils.GetCallbackURL`: `<scheme>://<host>/api/oauth_callback`.
pub(crate) fn callback_url(headers: &HeaderMap) -> String {
    let scheme = header_value(headers, "x-forwarded-proto")
        .or_else(|| header_value(headers, "x-forwarded-protocol"))
        .or_else(|| {
            header_value(headers, "x-forwarded-ssl")
                .filter(|value| value.eq_ignore_ascii_case("on"))
                .map(|_| "https".to_owned())
        })
        .or_else(|| header_value(headers, "x-url-scheme"))
        .unwrap_or_else(|| "http".to_owned());
    let host = header_value(headers, HOST.as_str()).unwrap_or_default();
    format!("{scheme}://{host}/api/oauth_callback")
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
