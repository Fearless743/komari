//! axum identity extraction and role enforcement.
//!
//! [`AuthContext`] is an extractor that performs the same identity resolution
//! as Go `api.IdentityMiddleware`:
//!
//! 1. `Authorization: Bearer <api_key>` (configured `api_key`), admin
//! 2. `session_token` cookie, admin
//! 3. agent token from `?token=<token>`, or from a JSON `token` field for
//!    non-GET requests (the body is buffered and restored), client
//! 4. everything else, guest
//!
//! `Authorization: Bearer` is only ever treated as the configured API key;
//! it is never used as an agent token, matching Go.
//!
//! [`RequireRole`] is a tower middleware that returns the Go-compatible
//! `401 {"status":"error","message":"Unauthorized."}` body when the resolved
//! role is not in the configured allow-list.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::header::{AUTHORIZATION, COOKIE, USER_AGENT};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
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
/// Config key enabling private-site mode.
const PRIVATE_SITE_CONFIG_KEY: &str = "private_site";
/// Config key holding the temporary share token.
const TEMP_SHARE_TOKEN_KEY: &str = "tempory_share_token";
/// Config key holding the temporary share expiry (Unix seconds).
const TEMP_SHARE_EXPIRE_KEY: &str = "tempory_share_token_expire_at";
/// Cookie carrying the temporary share token.
const TEMP_ACCESS_COOKIE: &str = "temp_key";

/// Path prefixes that bypass private-site checks, matching Go `publicPaths`.
/// `/api/admin` and `/api/clients/` are still protected by `RequireRole`.
pub const PUBLIC_PATH_PREFIXES: &[&str] = &[
    "/ping",
    "/api/public",
    "/api/login",
    "/api/me",
    "/api/oauth",
    "/api/oauth_callback",
    "/api/version",
    "/api/recent",
    "/api/admin",
    "/api/clients/",
];

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

/// Resolve the identity for a query-only request.
pub async fn authenticate_request(
    state: &AuthState,
    headers: &HeaderMap,
    uri: &Uri,
) -> AuthContext {
    authenticate_request_with(state, headers, uri, &Method::GET, None).await
}

/// Resolve the identity for a request, optionally with a buffered body.
pub async fn authenticate_request_with(
    state: &AuthState,
    headers: &HeaderMap,
    uri: &Uri,
    method: &Method,
    body: Option<&[u8]>,
) -> AuthContext {
    let authorization = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim);
    let bearer = authorization.and_then(|value| value.strip_prefix("Bearer "));

    // 1. API key (the only use of Authorization in Go).
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

    // 3. Agent token: query first, then the JSON body for non-GET requests.
    if let Some(token) = extract_client_token(uri.query(), method, body) {
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

/// Port of Go `api.extractClientToken`.
///
/// Query strings win. For non-GET requests the body is parsed as a JSON object
/// and its string `token` field is used; no Authorization header is consulted.
pub fn extract_client_token(
    query: Option<&str>,
    method: &Method,
    body: Option<&[u8]>,
) -> Option<String> {
    if let Some(token) = query.and_then(|query| query_param(query, "token")) {
        // Go `c.Query` returns the first value even when it is empty. An empty
        // first value does not fall through to later `token` parameters.
        if !token.is_empty() {
            return Some(token);
        }
    }
    // Go only skips body parsing for GET; HEAD keeps the generic path.
    if method == Method::GET {
        return None;
    }
    let body = body?;
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value
        .get("token")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

/// Buffer a non-GET body, resolve identity, and restore the request body so
/// downstream handlers can still read it.
async fn resolve_request_context(
    state: &AuthState,
    request: axum::extract::Request,
) -> (AuthContext, axum::extract::Request) {
    if let Some(context) = request.extensions().get::<AuthContext>().cloned() {
        return (context, request);
    }

    let (mut parts, body) = request.into_parts();
    let (auth_body, body): (Option<axum::body::Bytes>, Body) = if parts.method == Method::GET {
        (None, body)
    } else {
        match axum::body::to_bytes(body, usize::MAX).await {
            Ok(bytes) => {
                let restored = Body::from(bytes.clone());
                (Some(bytes), restored)
            }
            Err(_) => (None, Body::empty()),
        }
    };

    let context = authenticate_request_with(
        state,
        &parts.headers,
        &parts.uri,
        &parts.method,
        auth_body.as_deref(),
    )
    .await;
    parts.extensions.insert(context.clone());
    (context, axum::extract::Request::from_parts(parts, body))
}

/// The extractor resolves API keys, sessions and query tokens on its own, but
/// axum does not expose the request body to `FromRequestParts`. Put [`AuthLayer`],
/// [`RequireRole`] or [`PrivateSite`] in front of the route to authenticate a
/// non-GET JSON `token` field; the layer inserts this extractor's result into
/// request extensions and also restores the body for the handler.
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
        Ok(
            authenticate_request_with(&state, &parts.headers, &parts.uri, &parts.method, None)
                .await,
        )
    }
}

/// Body-preserving authentication middleware.
///
/// Resolves identity (including non-GET JSON body `token`) once, inserts the
/// resulting [`AuthContext`] into request extensions and restores the body.
/// Direct [`AuthContext`] / [`RequireClient`] extractors then read the cached
/// context, so POST body tokens work without adding a role restriction.
#[derive(Clone, Debug)]
pub struct AuthLayer {
    state: AuthState,
}

impl AuthLayer {
    pub fn new(state: AuthState) -> Self {
        Self { state }
    }
}

impl<S> Layer<S> for AuthLayer {
    type Service = AuthLayerService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthLayerService {
            inner,
            state: self.state.clone(),
        }
    }
}

/// Service produced by [`AuthLayer`].
#[derive(Clone, Debug)]
pub struct AuthLayerService<S> {
    inner: S,
    state: AuthState,
}

impl<S> Service<axum::extract::Request> for AuthLayerService<S>
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
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let (_, request) = resolve_request_context(&state, request).await;
            inner.call(request).await
        })
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
            let (context, request) = resolve_request_context(&state, request).await;
            if context.permits(&allowed) {
                inner.call(request).await
            } else {
                Ok(unauthorized_response())
            }
        })
    }
}

/// Private-site middleware, porting Go `api.PrivateSiteMiddleware`.
///
/// It must run after identity resolution (or will resolve identity itself).
/// Guests are rejected for `/api/*` paths while private-site mode is enabled,
/// except for the public prefixes and a valid temporary-share cookie.
#[derive(Clone, Debug)]
pub struct PrivateSite {
    state: AuthState,
}

impl PrivateSite {
    pub fn new(state: AuthState) -> Self {
        Self { state }
    }
}

impl<S> Layer<S> for PrivateSite {
    type Service = PrivateSiteService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PrivateSiteService {
            inner,
            state: self.state.clone(),
        }
    }
}

/// Service produced by [`PrivateSite`].
#[derive(Clone, Debug)]
pub struct PrivateSiteService<S> {
    inner: S,
    state: AuthState,
}

impl<S> Service<axum::extract::Request> for PrivateSiteService<S>
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
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let (context, request) = resolve_request_context(&state, request).await;
            if !context.is_guest() {
                return inner.call(request).await;
            }

            let path = request.uri().path().to_owned();
            if is_public_path(&path) || !path.starts_with("/api") {
                return inner.call(request).await;
            }

            let private_site = match state.db.config().get_as::<bool>(PRIVATE_SITE_CONFIG_KEY) {
                Ok(value) => value.unwrap_or(false),
                Err(_) => return Ok(internal_error_response()),
            };
            if !private_site || has_temp_access(&state, request.headers()) {
                return inner.call(request).await;
            }

            Ok(private_site_unauthorized_response())
        })
    }
}

/// Whether a path is in Go's public-prefix list.
pub fn is_public_path(path: &str) -> bool {
    PUBLIC_PATH_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

fn has_temp_access(state: &AuthState, headers: &HeaderMap) -> bool {
    let Some(cookie) = cookie_value(headers, TEMP_ACCESS_COOKIE) else {
        return false;
    };
    if cookie.is_empty() {
        return false;
    }
    let Ok(Some(allow_key)) = state.db.config().get_as::<String>(TEMP_SHARE_TOKEN_KEY) else {
        return false;
    };
    let Ok(Some(expire_at)) = state.db.config().get_as::<i64>(TEMP_SHARE_EXPIRE_KEY) else {
        return false;
    };
    !allow_key.is_empty() && cookie == allow_key && expire_at >= now_unix()
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

/// Go `RespondError(401, "Private site is enabled, please login first.")`.
pub fn private_site_unauthorized_response() -> Response {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"status":"error","message":"Private site is enabled, please login first."}"#,
        ))
        .expect("static 401 response is valid")
}

/// Go `RespondError(500, "Failed to get configuration.")`.
pub fn internal_error_response() -> Response {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"status":"error","message":"Failed to get configuration."}"#,
        ))
        .expect("static 500 response is valid")
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

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
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
    use axum::routing::{any, get, post};
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
    async fn query_token_works_for_agent_routes_but_not_admin_routes() {
        let fixture = fixture();
        let token = fixture.client_token.clone();

        let admin_request = axum::http::Request::builder()
            .uri(format!("/?token={token}"))
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
    async fn bearer_client_token_is_a_guest() {
        let fixture = fixture();
        let token = fixture.client_token.clone();
        let request = axum::http::Request::builder()
            .uri("/")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status(router(fixture.state, &[Role::Admin, Role::Client]), request).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn post_body_token_is_accepted_and_the_body_is_restored() {
        async fn handler(body: String) -> String {
            body
        }

        let fixture = fixture();
        let token = fixture.client_token.clone();
        let app = Router::new()
            .route("/", post(handler))
            .layer(RequireRole::new(
                fixture.state.clone(),
                [Role::Admin, Role::Client],
            ))
            .with_state(fixture.state);
        let payload = format!(r#"{{"token":"{token}"}}"#);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/")
            .body(Body::from(payload.clone()))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&body), payload);
    }

    #[test]
    fn extract_client_token_matches_go_precedence() {
        let method = Method::POST;
        let body: Option<&[u8]> = Some(br#"{"token":"body"}"#);
        assert_eq!(
            extract_client_token(Some("token=query"), &method, body),
            Some("query".to_owned())
        );
        assert_eq!(
            extract_client_token(None, &method, body),
            Some("body".to_owned())
        );
        assert_eq!(extract_client_token(None, &Method::GET, body), None);
        // Go only skips the body for GET.
        assert_eq!(
            extract_client_token(None, &Method::HEAD, body),
            Some("body".to_owned())
        );
        // The first query value wins even when empty; later values are ignored.
        assert_eq!(
            extract_client_token(Some("token=&token=second"), &method, body),
            Some("body".to_owned())
        );
        assert_eq!(
            extract_client_token(Some("token=&token=second"), &Method::GET, body),
            None
        );
        assert_eq!(
            extract_client_token(Some("token=first&token=second"), &method, body),
            Some("first".to_owned())
        );
        // The body token is not trimmed, matching Go.
        assert_eq!(
            extract_client_token(None, &method, Some(br#"{"token":"  spaced  "}"#.as_slice())),
            Some("  spaced  ".to_owned())
        );
        assert_eq!(extract_client_token(None, &method, Some(b"not-json")), None);
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

    #[tokio::test]
    async fn require_role_caches_body_token_for_direct_extractors() {
        async fn handler(context: AuthContext) -> String {
            assert!(context.client_uuid.is_some());
            context.role.to_string()
        }

        let fixture = fixture();
        let token = fixture.client_token.clone();
        let app = Router::new()
            .route("/", post(handler))
            .layer(RequireRole::new(
                fixture.state.clone(),
                [Role::Admin, Role::Client],
            ))
            .with_state(fixture.state);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/")
            .body(Body::from(format!(r#"{{"token":"{token}"}}"#)))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"client");
    }

    #[tokio::test]
    async fn private_site_caches_body_token_for_direct_extractors() {
        async fn handler(context: AuthContext) -> String {
            assert!(context.client_uuid.is_some());
            context.role.to_string()
        }

        let fixture = fixture();
        fixture
            .state
            .db()
            .config()
            .set(PRIVATE_SITE_CONFIG_KEY, &true)
            .unwrap();
        let token = fixture.client_token.clone();
        let app = Router::new()
            .route("/api/secret", post(handler))
            .layer(PrivateSite::new(fixture.state.clone()))
            .with_state(fixture.state);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/secret")
            .body(Body::from(format!(r#"{{"token":"{token}"}}"#)))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"client");
    }

    #[tokio::test]
    async fn head_body_token_passes_through_auth_layer() {
        async fn handler(context: AuthContext) -> String {
            context.role.to_string()
        }

        let fixture = fixture();
        let token = fixture.client_token.clone();
        let app = Router::new()
            .route("/", any(handler))
            .layer(AuthLayer::new(fixture.state.clone()))
            .with_state(fixture.state);
        let request = axum::http::Request::builder()
            .method("HEAD")
            .uri("/")
            .body(Body::from(format!(r#"{{"token":"{token}"}}"#)))
            .unwrap();
        assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn head_body_token_passes_through_require_role() {
        let fixture = fixture();
        let token = fixture.client_token.clone();
        let app = Router::new()
            .route("/", any(|| async { "ok" }))
            .layer(RequireRole::new(
                fixture.state.clone(),
                [Role::Admin, Role::Client],
            ))
            .with_state(fixture.state);
        let request = axum::http::Request::builder()
            .method("HEAD")
            .uri("/")
            .body(Body::from(format!(r#"{{"token":"{token}"}}"#)))
            .unwrap();
        assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn head_body_token_passes_through_private_site() {
        let fixture = fixture();
        fixture
            .state
            .db()
            .config()
            .set(PRIVATE_SITE_CONFIG_KEY, &true)
            .unwrap();
        let token = fixture.client_token.clone();
        let app = Router::new()
            .route("/api/secret", any(|| async { "ok" }))
            .layer(PrivateSite::new(fixture.state.clone()))
            .with_state(fixture.state);
        let request = axum::http::Request::builder()
            .method("HEAD")
            .uri("/api/secret")
            .body(Body::from(format!(r#"{{"token":"{token}"}}"#)))
            .unwrap();
        assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_layer_enables_body_token_for_direct_extractors() {
        async fn handler(context: AuthContext) -> String {
            assert!(context.client_uuid.is_some());
            context.role.to_string()
        }

        let fixture = fixture();
        let token = fixture.client_token.clone();
        let app = Router::new()
            .route("/", post(handler))
            .layer(AuthLayer::new(fixture.state.clone()))
            .with_state(fixture.state);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/")
            .body(Body::from(format!(r#"{{"token":"{token}"}}"#)))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"client");
    }

    #[tokio::test]
    async fn require_client_extractor_reads_body_token_behind_auth_layer() {
        async fn handler(client: RequireClient) -> String {
            client.0.role.to_string()
        }

        let fixture = fixture();
        let token = fixture.client_token.clone();
        let app = Router::new()
            .route("/", post(handler))
            .layer(AuthLayer::new(fixture.state.clone()))
            .with_state(fixture.state);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/")
            .body(Body::from(format!(r#"{{"token":"{token}"}}"#)))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"client");
    }

    fn private_router(state: AuthState) -> Router {
        Router::new()
            .route("/api/secret", get(|| async { "ok" }))
            .route("/api/admin/x", get(|| async { "ok" }))
            .route("/api/clients/report", get(|| async { "ok" }))
            .route("/ping", get(|| async { "pong" }))
            .route("/api/login", get(|| async { "login" }))
            .route("/api/public/x", get(|| async { "public" }))
            .route("/assets/app.js", get(|| async { "asset" }))
            .layer(PrivateSite::new(state.clone()))
            .with_state(state)
    }

    fn private_fixture() -> Fixture {
        let fixture = fixture();
        fixture
            .state
            .db()
            .config()
            .set(PRIVATE_SITE_CONFIG_KEY, &true)
            .unwrap();
        fixture
    }

    #[tokio::test]
    async fn private_site_rejects_guest_api_paths_but_allows_public_paths() {
        let fixture = private_fixture();
        let secret = axum::http::Request::builder()
            .uri("/api/secret")
            .body(Body::empty())
            .unwrap();
        let response = private_router(fixture.state.clone())
            .oneshot(secret)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["message"],
            "Private site is enabled, please login first."
        );

        for path in [
            "/ping",
            "/assets/app.js",
            "/api/login",
            "/api/public/x",
            "/api/admin/x",
            "/api/clients/report",
        ] {
            let request = axum::http::Request::builder()
                .uri(path)
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                status(private_router(fixture.state.clone()), request).await,
                StatusCode::OK,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn private_site_reports_configuration_failures_as_500() {
        let fixture = fixture();
        fixture
            .state
            .db()
            .config()
            .set_raw(PRIVATE_SITE_CONFIG_KEY, "not-json")
            .unwrap();
        let request = axum::http::Request::builder()
            .uri("/api/secret")
            .body(Body::empty())
            .unwrap();
        let response = private_router(fixture.state)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&body),
            r#"{"status":"error","message":"Failed to get configuration."}"#
        );
    }

    #[tokio::test]
    async fn private_site_allows_sessions_and_temporary_cookies() {
        let fixture = private_fixture();

        let session_request = axum::http::Request::builder()
            .uri("/api/secret")
            .header(COOKIE, format!("{SESSION_COOKIE_NAME}={}", fixture.session))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status(private_router(fixture.state.clone()), session_request).await,
            StatusCode::OK
        );

        fixture
            .state
            .db()
            .config()
            .set(TEMP_SHARE_TOKEN_KEY, &"share-token")
            .unwrap();
        fixture
            .state
            .db()
            .config()
            .set(TEMP_SHARE_EXPIRE_KEY, &(now_unix() + 60))
            .unwrap();

        let temp_request = axum::http::Request::builder()
            .uri("/api/secret")
            .header(COOKIE, format!("{TEMP_ACCESS_COOKIE}=share-token"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status(private_router(fixture.state.clone()), temp_request).await,
            StatusCode::OK
        );

        fixture
            .state
            .db()
            .config()
            .set(TEMP_SHARE_EXPIRE_KEY, &(now_unix() - 1))
            .unwrap();
        let expired_request = axum::http::Request::builder()
            .uri("/api/secret")
            .header(COOKIE, format!("{TEMP_ACCESS_COOKIE}=share-token"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status(private_router(fixture.state), expired_request).await,
            StatusCode::UNAUTHORIZED
        );
    }
}
