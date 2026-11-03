//! axum identity extraction and role enforcement.
//!
//! [`AuthContext`] is an extractor that performs the same identity resolution
//! as Go `api.IdentityMiddleware`:
//!
//! 1. `Authorization: Bearer <api_key>` (configured `api_key`), admin
//! 2. `session_token` cookie, admin
//! 3. agent token from `Authorization: Bearer <token>` or `?token=<token>`,
//!    client
//! 4. everything else, guest
//!
//! [`RequireRole`] is a tower middleware that returns the Go-compatible
//! `401 {"status":"error","message":"Unauthorized."}` body when the resolved
//! role is not in the configured allow-list.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::header::{AUTHORIZATION, COOKIE, USER_AGENT};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use komari_db::Db;
use tower::{Layer, Service};

use crate::role::Role;
use crate::session::validate_session_with;

/// Name of the session cookie set by the Go login handler.
pub const SESSION_COOKIE_NAME: &str = "session_token";
/// UUID reported for API-key requests (Go `IdentityMiddleware`).
pub const API_KEY_UUID: &str = "00000000-0000-0000-0000-000000000000";
/// Config key holding the administrator API key.
const API_KEY_CONFIG_KEY: &str = "api_key";
/// The Go middleware requires API keys to be at least this long.
const MIN_API_KEY_LENGTH: usize = 12;

/// Shared state needed to authenticate a request.
#[derive(Clone, Debug)]
pub struct AuthState {
    db: Arc<Db>,
    /// Optional pre-loaded API key. When `None`, the `api_key` config row is
    /// consulted on every request (the Go middleware does the same).
    api_key: Option<String>,
}

impl AuthState {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db, api_key: None }
    }

    /// Build state from an owned handle (useful in tests).
    pub fn from_db(db: Db) -> Self {
        Self::new(Arc::new(db))
    }

    /// Override the API key instead of reading it from the config table.
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    fn resolved_api_key(&self) -> Option<String> {
        let key = match &self.api_key {
            Some(key) => Some(key.clone()),
            None => self
                .db
                .config()
                .get_as::<String>(API_KEY_CONFIG_KEY)
                .ok()
                .flatten(),
        }?;
        (key.len() >= MIN_API_KEY_LENGTH).then_some(key)
    }
}

impl From<Arc<Db>> for AuthState {
    fn from(db: Arc<Db>) -> Self {
        Self::new(db)
    }
}

/// Identity resolved for one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthContext {
    pub role: Role,
    /// UUID of the session user, or [`API_KEY_UUID`] for API-key requests.
    pub user_uuid: Option<String>,
    /// Username for session-validated requests.
    pub username: Option<String>,
    /// Session cookie value when the request used a session.
    pub session_token: Option<String>,
    /// Agent/device UUID when the request used a client token.
    pub client_uuid: Option<String>,
    /// The API key when the request used API-key authentication.
    pub api_key: Option<String>,
}

impl AuthContext {
    pub fn guest() -> Self {
        Self {
            role: Role::Guest,
            user_uuid: None,
            username: None,
            session_token: None,
            client_uuid: None,
            api_key: None,
        }
    }

    pub fn is_guest(&self) -> bool {
        self.role == Role::Guest
    }

    pub fn is_client(&self) -> bool {
        self.role == Role::Client
    }

    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }

    pub fn permits(&self, allowed: &[Role]) -> bool {
        self.role.allows(allowed)
    }
}

/// Resolve the identity for a request. Returns a guest context on any
/// authentication/database error, matching the Go middleware's silent fallback.
pub async fn authenticate_request(
    state: &AuthState,
    headers: &HeaderMap,
    uri: &Uri,
) -> AuthContext {
    let authorization = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim);
    let bearer = authorization.and_then(|value| value.strip_prefix("Bearer "));

    // 1. API key.
    if let (Some(configured), Some(token)) = (state.resolved_api_key(), bearer) {
        if token == configured {
            return AuthContext {
                role: Role::Admin,
                user_uuid: Some(API_KEY_UUID.to_owned()),
                username: None,
                session_token: None,
                client_uuid: None,
                api_key: Some(token.to_owned()),
            };
        }
    }

    // 2. Session cookie.
    if let Some(token) = cookie_value(headers, SESSION_COOKIE_NAME) {
        if !token.is_empty() {
            let user_agent = headers
                .get(USER_AGENT)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            let ip = client_ip(headers).unwrap_or_default();
            if let Ok(Some(validation)) = validate_session_with(&state.db, &token, user_agent, &ip)
            {
                return AuthContext {
                    role: validation.role,
                    user_uuid: Some(validation.user.uuid),
                    username: Some(validation.user.username),
                    session_token: Some(token),
                    client_uuid: None,
                    api_key: None,
                };
            }
        }
    }

    // 3. Agent token: Authorization Bearer first, then ?token=.
    let client_token = bearer
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .or_else(|| query_param(uri, "token"));
    if let Some(token) = client_token {
        if let Ok(Some(client)) = state.db.clients().get_by_token(&token) {
            return AuthContext {
                role: Role::Client,
                user_uuid: None,
                username: None,
                session_token: None,
                client_uuid: Some(client.uuid),
                api_key: None,
            };
        }
    }

    AuthContext::guest()
}

impl<S> FromRequestParts<S> for AuthContext
where
    S: Send + Sync,
    AuthState: FromRef<S>,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if let Some(context) = parts.extensions.get::<AuthContext>() {
            return Ok(context.clone());
        }
        let state = AuthState::from_ref(state);
        Ok(authenticate_request(&state, &parts.headers, &parts.uri).await)
    }
}

/// Tower middleware enforcing an allow-list of roles.
#[derive(Clone, Debug)]
pub struct RequireRole {
    state: AuthState,
    allowed: Arc<Vec<Role>>,
}

impl RequireRole {
    /// Allow exactly `allowed` roles.
    pub fn new(state: AuthState, allowed: impl IntoIterator<Item = Role>) -> Self {
        Self {
            state,
            allowed: Arc::new(allowed.into_iter().collect()),
        }
    }

    /// Convenience for admin-only routes.
    pub fn admin(state: AuthState) -> Self {
        Self::new(state, [Role::Admin])
    }

    /// Convenience for the Go `/api/clients` group (admin or client).
    pub fn admin_or_client(state: AuthState) -> Self {
        Self::new(state, [Role::Admin, Role::Client])
    }
}

impl<S> Layer<S> for RequireRole {
    type Service = RequireRoleService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequireRoleService {
            inner,
            state: self.state.clone(),
            allowed: Arc::clone(&self.allowed),
        }
    }
}

/// Service produced by [`RequireRole`].
#[derive(Clone, Debug)]
pub struct RequireRoleService<S> {
    inner: S,
    state: AuthState,
    allowed: Arc<Vec<Role>>,
}

impl<S> Service<axum::extract::Request> for RequireRoleService<S>
where
    S: Service<axum::extract::Request, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: axum::extract::Request) -> Self::Future {
        let state = self.state.clone();
        let allowed = Arc::clone(&self.allowed);
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let context = authenticate_request(&state, request.headers(), request.uri()).await;
            if context.permits(&allowed) {
                let mut request = request;
                request.extensions_mut().insert(context);
                inner.call(request).await
            } else {
                Ok(unauthorized_response())
            }
        })
    }
}

/// Rejection returned by the role guard extractors.
#[derive(Debug, Clone, Copy)]
pub struct Unauthorized;

impl IntoResponse for Unauthorized {
    fn into_response(self) -> Response {
        unauthorized_response()
    }
}

/// The exact error body used by Go `api.RespondError(..., "Unauthorized.")`.
pub fn unauthorized_response() -> Response {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"status":"error","message":"Unauthorized."}"#,
        ))
        .expect("static 401 response is valid")
}

/// Extractor that accepts only administrators.
#[derive(Clone, Debug)]
pub struct RequireAdmin(pub AuthContext);

impl<S> FromRequestParts<S> for RequireAdmin
where
    S: Send + Sync,
    AuthState: FromRef<S>,
{
    type Rejection = Unauthorized;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let context = AuthContext::from_request_parts(parts, state)
            .await
            .expect("AuthContext extraction is infallible");
        if context.is_admin() {
            Ok(Self(context))
        } else {
            Err(Unauthorized)
        }
    }
}

/// Extractor matching the Go `/api/clients` group (admin or client).
#[derive(Clone, Debug)]
pub struct RequireClient(pub AuthContext);

impl<S> FromRequestParts<S> for RequireClient
where
    S: Send + Sync,
    AuthState: FromRef<S>,
{
    type Rejection = Unauthorized;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let context = AuthContext::from_request_parts(parts, state)
            .await
            .expect("AuthContext extraction is infallible");
        if context.permits(&[Role::Admin, Role::Client]) {
            Ok(Self(context))
        } else {
            Err(Unauthorized)
        }
    }
}

/// Extractor requiring any authenticated identity (not a guest).
#[derive(Clone, Debug)]
pub struct RequireAuthenticated(pub AuthContext);

impl<S> FromRequestParts<S> for RequireAuthenticated
where
    S: Send + Sync,
    AuthState: FromRef<S>,
{
    type Rejection = Unauthorized;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let context = AuthContext::from_request_parts(parts, state)
            .await
            .expect("AuthContext extraction is infallible");
        if context.is_guest() {
            Err(Unauthorized)
        } else {
            Ok(Self(context))
        }
    }
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
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

fn query_param(uri: &Uri, name: &str) -> Option<String> {
    uri.query()?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(key) == name).then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        decoded.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        decoded.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn client_ip(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
    {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::routing::get;
    use komari_db::models::Client;
    use tower::ServiceExt;

    use super::*;

    struct Fixture {
        state: AuthState,
        session: String,
        client_token: String,
    }

    fn fixture() -> Fixture {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let user = db.accounts().create("admin", "password").unwrap();
        let session =
            crate::session::create_session(&db, &user.uuid, "test-agent", "127.0.0.1", "password")
                .unwrap();
        let client = Client::new("node-1");
        db.clients().create(&client).unwrap();
        Fixture {
            state: AuthState::from_db(db),
            session,
            client_token: client.token,
        }
    }

    fn router(state: AuthState, allowed: &[Role]) -> Router {
        Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(RequireRole::new(state.clone(), allowed.iter().copied()))
            .with_state(state)
    }

    async fn status(router: Router, request: axum::http::Request<Body>) -> StatusCode {
        router.oneshot(request).await.unwrap().status()
    }

    fn request() -> axum::http::Request<Body> {
        axum::http::Request::builder()
            .uri("/")
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn guest_is_rejected_from_protected_routes() {
        let fixture = fixture();
        let response = router(fixture.state, &[Role::Admin])
            .oneshot(request())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "error");
        assert_eq!(json["message"], "Unauthorized.");
    }

    #[tokio::test]
    async fn admin_session_is_allowed() {
        let fixture = fixture();
        let request = axum::http::Request::builder()
            .uri("/")
            .header(COOKIE, format!("{SESSION_COOKIE_NAME}={}", fixture.session))
            .header(USER_AGENT, "test-browser")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status(router(fixture.state, &[Role::Admin]), request).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn client_token_works_for_agent_routes_but_not_admin_routes() {
        let fixture = fixture();
        let token = fixture.client_token.clone();

        let admin_request = axum::http::Request::builder()
            .uri("/")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status(router(fixture.state.clone(), &[Role::Admin]), admin_request).await,
            StatusCode::UNAUTHORIZED
        );

        let agent_request = axum::http::Request::builder()
            .uri(format!("/?token={token}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status(
                router(fixture.state, &[Role::Admin, Role::Client]),
                agent_request
            )
            .await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn configured_api_key_is_admin() {
        let fixture = fixture();
        let state = fixture.state.with_api_key("0123456789abcdef");
        let request = axum::http::Request::builder()
            .uri("/")
            .header(AUTHORIZATION, "Bearer 0123456789abcdef")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status(router(state, &[Role::Admin]), request).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn auth_context_extractor_reports_the_role() {
        async fn handler(context: AuthContext) -> String {
            format!(
                "{}:{}",
                context.role,
                context.username.unwrap_or_else(|| "anonymous".to_owned())
            )
        }

        let fixture = fixture();
        let app = Router::new()
            .route("/", get(handler))
            .with_state(fixture.state.clone());
        let request = axum::http::Request::builder()
            .uri("/")
            .header(COOKIE, format!("{SESSION_COOKIE_NAME}={}", fixture.session))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"admin:admin");
    }
}
