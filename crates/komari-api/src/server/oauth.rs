//! HTTP assembly for `/api/oauth` and `/api/oauth_callback`.
//!
//! The handlers mirror `api/public/oauth.go`:
//!
//! * `/api/oauth` is a 403 unless `o_auth_enabled` is true, sets the
//!   `oauth_state` cookie and redirects to the provider;
//! * `/api/oauth_callback` validates the cookie/query state, exchanges the
//!   code through [`komari_oauth::OAuthProvider`], then either binds the
//!   external identity to the logged-in account (the
//!   `binding_external_account` cookie) or logs in the already-bound account;
//! * an unknown external identity returns `401` with Go's message instead of
//!   creating an account.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use komari_db::Db;
use komari_oauth::{
    AccountLinker, CallbackQuery, CallbackRequest, OAuthError, OAuthProvider, SsoIdentity,
};
use serde_json::{Value, json};

/// Name of the state cookie used by the Go backend.
pub const OAUTH_STATE_COOKIE: &str = "oauth_state";
/// Cookie set by the admin binding endpoint (`GET /api/admin/oauth2/bind`).
pub const BINDING_COOKIE: &str = "binding_external_account";
/// Session cookie name, matching [`komari_auth::SESSION_COOKIE_NAME`].
pub const SESSION_COOKIE: &str = "session_token";
/// Go's OAuth state cookie lifetime (one hour).
const STATE_COOKIE_MAX_AGE: i64 = 3600;
/// Go's OAuth session lifetime (30 days).
const SESSION_MAX_AGE: i64 = 2_592_000;

/// Provider manager used by the HTTP routes.
///
/// Production uses the process-wide [`komari_oauth::global`] manager so the
/// admin API's `LoadProvider` calls take effect immediately. Tests can install
/// an explicit provider with [`OAuthRuntime::with_provider`].
#[derive(Clone)]
pub struct OAuthRuntime {
    provider: Arc<RwLock<Option<Arc<dyn OAuthProvider>>>>,
    callback_base: Option<String>,
}

impl std::fmt::Debug for OAuthRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthRuntime")
            .field("provider", &self.current_name())
            .field("callback_base", &self.callback_base)
            .finish()
    }
}

impl Default for OAuthRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthRuntime {
    /// Use the process-wide OAuth manager.
    pub fn new() -> Self {
        Self {
            provider: Arc::new(RwLock::new(None)),
            callback_base: None,
        }
    }

    /// Use an explicit provider (tests and custom assemblies).
    pub fn with_provider(provider: Arc<dyn OAuthProvider>) -> Self {
        Self {
            provider: Arc::new(RwLock::new(Some(provider))),
            callback_base: None,
        }
    }

    /// Override the callback URL instead of deriving it from `Host` and
    /// `X-Forwarded-*` headers.
    pub fn with_callback_base(mut self, callback_base: impl Into<String>) -> Self {
        self.callback_base = Some(callback_base.into());
        self
    }

    /// Install or clear the explicit provider.
    pub fn set_provider(&self, provider: Option<Arc<dyn OAuthProvider>>) {
        *self
            .provider
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = provider;
    }

    /// The provider used for the next request.
    pub fn current(&self) -> Option<Arc<dyn OAuthProvider>> {
        self.provider
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .or_else(komari_oauth::current_provider)
    }

    /// Provider name for diagnostics.
    pub fn current_name(&self) -> Option<&'static str> {
        self.current().map(|provider| provider.name())
    }

    /// Seed default provider rows and load the configured provider.
    pub async fn initialize_from_db(&self, db: &Db) -> Result<(), OAuthError> {
        komari_oauth::global().initialize_from_db(db).await
    }

    fn callback_url(&self, headers: &HeaderMap) -> String {
        if let Some(base) = self.callback_base.as_deref() {
            return base.to_owned();
        }
        let scheme = forwarded_scheme(headers);
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        format!("{scheme}://{host}/api/oauth_callback")
    }
}

#[derive(Clone)]
struct OAuthState {
    db: Arc<Db>,
    runtime: OAuthRuntime,
}

/// Routes owned by this module.
pub fn router(db: Arc<Db>, runtime: OAuthRuntime) -> Router {
    Router::new()
        .route("/api/oauth", get(oauth_start))
        .route("/api/oauth_callback", get(oauth_callback))
        .with_state(OAuthState { db, runtime })
}

async fn oauth_start(State(state): State<OAuthState>, headers: HeaderMap) -> Response {
    let enabled = state
        .db
        .config()
        .get_as::<bool>("o_auth_enabled")
        .ok()
        .flatten()
        .unwrap_or(false);
    if !enabled {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"status": "error", "error": "OAuth is not enabled"})),
        )
            .into_response();
    }

    let Some(provider) = state.runtime.current() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"status": "error", "error": "OAuth provider is not initialized"})),
        )
            .into_response();
    };

    let redirect_uri = state.runtime.callback_url(&headers);
    let authorization = match provider.authorization_url(&redirect_uri).await {
        Ok(authorization) => authorization,
        Err(error) => {
            tracing::warn!(%error, "failed to build OAuth authorization URL");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "status": "error",
                    "error": format!("Failed to get authorization URL: {error}"),
                })),
            )
                .into_response();
        }
    };

    let cookie = format!(
        "{OAUTH_STATE_COOKIE}={}; Path=/; Max-Age={STATE_COOKIE_MAX_AGE}; HttpOnly",
        authorization.state
    );
    let mut response = Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, authorization.url)
        .body(axum::body::Body::empty())
        .expect("valid OAuth redirect");
    append_cookie(&mut response, &cookie);
    response
}

async fn oauth_callback(
    State(state): State<OAuthState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let cookies = Cookies::parse(&headers);
    let state_cookie = cookies.get(OAUTH_STATE_COOKIE).unwrap_or_default();
    let clear_state = format!("{OAUTH_STATE_COOKIE}=; Path=/; Max-Age=0; HttpOnly");
    let clear_binding = format!("{BINDING_COOKIE}=; Path=/; Max-Age=0; HttpOnly");

    let Some(provider) = state.runtime.current() else {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "error",
            "OAuth provider is not initialized",
            std::slice::from_ref(&clear_state),
        );
    };

    let query_state = query.get("state").map(String::as_str);
    if provider.skip_query_state_check() {
        if state_cookie.is_empty() {
            return json_error(
                StatusCode::BAD_REQUEST,
                "error",
                "Invalid state",
                std::slice::from_ref(&clear_state),
            );
        }
    } else if state_cookie.is_empty() || query_state != Some(state_cookie.as_str()) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "error",
            "Invalid state",
            std::slice::from_ref(&clear_state),
        );
    }

    let redirect_uri = state.runtime.callback_url(&headers);
    let callback_query: CallbackQuery = query.clone();
    let user = match provider
        .callback(CallbackRequest {
            state: &state_cookie,
            query_state,
            code: query.get("code").map(String::as_str),
            redirect_uri: &redirect_uri,
            query: &callback_query,
            headers: &headers,
        })
        .await
    {
        Ok(user) => user,
        Err(error) => {
            tracing::warn!(%error, "OAuth callback failed");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "error",
                &format!("Failed to get user info: {error}"),
                std::slice::from_ref(&clear_state),
            );
        }
    };

    let identity = SsoIdentity::new(user.provider.clone(), user.subject.clone());
    let linker = AccountLinker::new(&state.db);
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let ip = client_ip(&headers);

    // Binding flow: the admin endpoint stores the target UUID in a cookie and
    // redirects here. The session must still belong to that account.
    if let Some(binding_uuid) = cookies
        .get(BINDING_COOKIE)
        .filter(|value| !value.is_empty())
    {
        let session = cookies.get(SESSION_COOKIE).unwrap_or_default();
        let valid = komari_auth::validate_session(&state.db, &session)
            .ok()
            .flatten()
            .is_some_and(|validation| validation.user.uuid == binding_uuid);
        if !valid {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "message",
                "Binding failed",
                &[clear_state.clone(), clear_binding],
            );
        }
        // Go's `BindingExternalAccount` overwrites the account's `sso_id`.
        // `rebind` preserves that behaviour while still refusing an identity
        // that already belongs to a different account.
        if linker.rebind(&binding_uuid, &identity).is_err() {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "message",
                "Binding failed",
                &[clear_state.clone(), clear_binding],
            );
        }
        let _ = state.db.logs().log(
            &ip,
            &binding_uuid,
            format!(
                "bound external account (OAuth),sso_id: {}",
                identity.combined_id()
            ),
            "login",
        );
        return redirect_with_cookies("/admin", &[clear_state, clear_binding]);
    }

    let session_user = match linker.login(&identity) {
        Ok(user) => user,
        Err(OAuthError::AccountNotBound) => {
            return json_error(
                StatusCode::UNAUTHORIZED,
                "message",
                "please log in and bind your external account first.",
                &[clear_state.clone(), clear_binding],
            );
        }
        Err(error) => {
            tracing::warn!(%error, "OAuth account lookup failed");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "message",
                &error.to_string(),
                &[clear_state.clone(), clear_binding],
            );
        }
    };

    let session = match komari_auth::create_session_with_expiry(
        &state.db,
        &session_user.uuid,
        SESSION_MAX_AGE,
        user_agent,
        &ip,
        "oauth",
    ) {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(%error, "failed to create OAuth session");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "message",
                &error.to_string(),
                &[clear_state.clone(), clear_binding],
            );
        }
    };

    let _ = state
        .db
        .logs()
        .log(&ip, &session_user.uuid, "logged in (OAuth)", "login");
    let session_cookie =
        format!("{SESSION_COOKIE}={session}; Path=/; Max-Age={SESSION_MAX_AGE}; HttpOnly");
    redirect_with_cookies("/admin", &[clear_state, clear_binding, session_cookie])
}

fn forwarded_scheme(headers: &HeaderMap) -> String {
    for name in ["x-forwarded-proto", "x-forwarded-protocol", "x-url-scheme"] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_owned();
            }
        }
    }
    if headers
        .get("x-forwarded-ssl")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("on"))
    {
        return "https".to_owned();
    }
    "http".to_owned()
}

fn client_ip(headers: &HeaderMap) -> String {
    if let Some(value) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return value.to_owned();
    }
    headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_owned()
}

struct Cookies(HashMap<String, String>);

impl Cookies {
    fn parse(headers: &HeaderMap) -> Self {
        let mut values = HashMap::new();
        for header in headers.get_all(header::COOKIE) {
            let Ok(header) = header.to_str() else {
                continue;
            };
            for pair in header.split(';') {
                let Some((key, value)) = pair.trim().split_once('=') else {
                    continue;
                };
                values
                    .entry(key.to_owned())
                    .or_insert_with(|| value.to_owned());
            }
        }
        Self(values)
    }

    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

fn json_error(status: StatusCode, field: &str, message: &str, cookies: &[String]) -> Response {
    let mut body = serde_json::Map::new();
    body.insert("status".to_owned(), json!("error"));
    body.insert(field.to_owned(), json!(message));
    let mut response = (status, Json(Value::Object(body))).into_response();
    for cookie in cookies {
        append_cookie(&mut response, cookie);
    }
    response
}

fn redirect_with_cookies(location: &str, cookies: &[String]) -> Response {
    let mut response = Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .body(axum::body::Body::empty())
        .expect("valid redirect");
    for cookie in cookies {
        append_cookie(&mut response, cookie);
    }
    response
}

fn append_cookie(response: &mut Response, cookie: &str) {
    if let Ok(value) = HeaderValue::from_str(cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}
