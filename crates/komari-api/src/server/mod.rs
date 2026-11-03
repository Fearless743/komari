//! HTTP server assembly.
//!
//! This module ports the wiring in `cmd/server.go`: it opens and migrates the
//! database, restores a pending `data/backup.zip`, initializes GeoIP, DDNS,
//! notifications and OAuth, mounts the agent/browser routes, the OAuth routes
//! and the static web assets, then starts the background loops.
//!
//! # B04/B06 integration
//!
//! The public API (B04) and the admin/JSON-RPC API (B06) are developed in
//! parallel. Their routers can be attached without changing this file:
//!
//! ```no_run
//! # use axum::Router;
//! # use komari_api::{ApiState, server::ServerBuilder};
//! # fn wire(builder: ServerBuilder, public: Router<ApiState>, admin: Router<ApiState>, rpc: Router<ApiState>) -> ServerBuilder {
//! builder
//!     .merge_public(public)
//!     .merge_admin(admin)
//!     .merge_rpc(rpc)
//! # }
//! ```
//!
//! The routers must use [`ApiState`]; they are merged before `with_state` is
//! applied. A stateless router can be attached with
//! [`ServerBuilder::merge_stateless`].

mod assets;
mod background;
mod oauth;

pub use assets::{
    TEMP_SHARE_EXPIRE_KEY, TEMP_SHARE_TOKEN_KEY, build_web_assets, data_dir_for_database,
    refresh_web_assets,
};
pub use background::{BackgroundConfig, BackgroundTasks};
pub use oauth::{BINDING_COOKIE, OAUTH_STATE_COOKIE, OAuthRuntime, SESSION_COOKIE};

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use komari_auth::{AuthLayer, PrivateSite};
use komari_config::Config;
use komari_db::{BackupOptions, Db, DbError, restore_pending_backup};
use komari_ddns::DdnsService;
use komari_geoip::GeoIpService;
use komari_web::WebAssets;
use tokio::net::TcpListener;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::{DefaultOnResponse, MakeSpan, TraceLayer};
use tracing::{Level, error, info, warn};

use crate::client_router_with_state;
use crate::state::{ApiConfig, ApiState};

/// Default listen address, matching the Go backend.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:25774";
/// Default SQLite database path, matching the Go backend.
pub const DEFAULT_DB_FILE: &str = "data/komari.db";
/// Default request-body limit (16 MiB).
///
/// The limit is a streaming layer applied before authentication, so it cannot
/// be overridden per route. Deployments that accept large backup uploads
/// through B06 should raise [`ServerConfig::body_limit_bytes`] (the Go backend
/// has no such limit) or terminate uploads at a reverse proxy.
pub const DEFAULT_BODY_LIMIT: usize = 16 * 1024 * 1024;

/// Startup configuration for the HTTP server.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub listen: String,
    pub database_path: PathBuf,
    /// Data directory containing themes, favicon and `backup.zip`.
    pub data_dir: PathBuf,
    pub theme: String,
    pub api: ApiConfig,
    pub background: BackgroundConfig,
    pub body_limit_bytes: usize,
    /// Install tower-http's request/response tracing layer.
    pub request_logging: bool,
    /// Maximum time graceful shutdown waits for half-open connections.
    pub shutdown_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: DEFAULT_LISTEN.to_owned(),
            database_path: PathBuf::from(DEFAULT_DB_FILE),
            data_dir: PathBuf::from("data"),
            theme: "default".to_owned(),
            api: ApiConfig::default(),
            background: BackgroundConfig::default(),
            body_limit_bytes: DEFAULT_BODY_LIMIT,
            request_logging: true,
            shutdown_timeout: Duration::from_secs(5),
        }
    }
}

impl ServerConfig {
    /// Convert the CLI/startup configuration.
    pub fn from_app_config(config: &Config) -> Self {
        let database_path = PathBuf::from(&config.db.file);
        let data_dir = if config.db.file == ":memory:" {
            PathBuf::from("data")
        } else {
            data_dir_for_database(&database_path)
        };
        Self {
            listen: config.listen.clone(),
            database_path,
            data_dir,
            theme: config.theme.clone(),
            api: ApiConfig {
                allow_cors: config.allow_cors,
                ws_disable_origin: config.ws_disable_origin,
                ..ApiConfig::default()
            },
            ..Self::default()
        }
    }
}

/// Builds a [`Server`] with optional pre-opened database/state and B04/B06
/// routers.
pub struct ServerBuilder {
    config: ServerConfig,
    db: Option<Db>,
    state: Option<ApiState>,
    assets: Option<Arc<WebAssets>>,
    oauth: Option<OAuthRuntime>,
    public_router: Option<Router<ApiState>>,
    admin_router: Option<Router<ApiState>>,
    rpc_router: Option<Router<ApiState>>,
    stateless_router: Option<Router>,
    state_hook: Option<Box<dyn FnOnce(ApiState) -> ApiState + Send>>,
    initialize_services: bool,
}

impl ServerBuilder {
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config,
            db: None,
            state: None,
            assets: None,
            oauth: None,
            public_router: None,
            admin_router: None,
            rpc_router: None,
            stateless_router: None,
            state_hook: None,
            initialize_services: true,
        }
    }

    /// Use an already opened and migrated database.
    pub fn with_db(mut self, db: Db) -> Self {
        self.db = Some(db);
        self
    }

    /// Use an already constructed API state (its database is reused).
    pub fn with_state(mut self, state: ApiState) -> Self {
        self.state = Some(state);
        self
    }

    pub fn with_web_assets(mut self, assets: Arc<WebAssets>) -> Self {
        self.assets = Some(assets);
        self
    }

    pub fn with_oauth(mut self, oauth: OAuthRuntime) -> Self {
        self.oauth = Some(oauth);
        self
    }

    /// Skip GeoIP/DDNS/notification/OAuth initialization. Tests and custom
    /// assemblies use this when those services are not needed.
    pub fn skip_services(mut self) -> Self {
        self.initialize_services = false;
        self
    }

    /// Attach the B04 public API router (`Router<ApiState>`).
    pub fn merge_public(mut self, router: Router<ApiState>) -> Self {
        self.public_router = Some(router);
        self
    }

    /// Attach the B06 admin API router (`Router<ApiState>`).
    pub fn merge_admin(mut self, router: Router<ApiState>) -> Self {
        self.admin_router = Some(router);
        self
    }

    /// Attach the B04 JSON-RPC router (`Router<ApiState>`).
    pub fn merge_rpc(mut self, router: Router<ApiState>) -> Self {
        self.rpc_router = Some(router);
        self
    }

    /// Attach a router that already has its state applied.
    pub fn merge_stateless(mut self, router: Router) -> Self {
        self.stateless_router = Some(router);
        self
    }

    /// Customize the shared state after the server services (web assets, DDNS,
    /// OAuth provider) have been attached and before the router is built.
    ///
    /// B04/B06 can use this to install their own service handles or middleware
    /// state without changing the assembly.
    pub fn configure_state<F>(mut self, hook: F) -> Self
    where
        F: FnOnce(ApiState) -> ApiState + Send + 'static,
    {
        self.state_hook = Some(Box::new(hook));
        self
    }

    pub async fn build(self) -> Result<Server> {
        let config = self.config;
        let mut state = match (self.state, self.db) {
            (Some(state), _) => state,
            (None, Some(db)) => ApiState::with_config(db, config.api.clone()),
            (None, None) => {
                let db = prepare_database(&config)?;
                ApiState::with_config(db, config.api.clone())
            }
        };

        let geoip = match state.geoip.clone() {
            Some(service) => service,
            None => Arc::new(GeoIpService::new().with_database(state.db.clone())),
        };
        if self.initialize_services {
            if let Err(error) = geoip.initialize().await {
                warn!(%error, "failed to initialize GeoIP; continuing without lookups");
            }
        }
        state = state.with_geoip_service(geoip);

        let db = state.db.clone();
        let oauth = self.oauth.unwrap_or_default();
        if self.initialize_services {
            if let Err(error) = oauth.initialize_from_db(&db).await {
                warn!(%error, "failed to initialize the OAuth provider");
            }
            if let Err(error) = komari_notify::initialize_defaults(&db) {
                warn!(%error, "failed to initialize notification providers");
            }
        }

        let ddns = Arc::new(DdnsService::new(db.clone()));
        if self.initialize_services {
            if let Err(error) = ddns.initialize().await {
                warn!(%error, "failed to initialize DDNS");
            }
        }

        let assets = match self.assets {
            Some(assets) => assets,
            None => build_web_assets(&config, &db).context("failed to load web assets")?,
        };

        state = state
            .with_web_assets(assets.clone())
            .with_ddns_service(ddns.clone());
        state.set_oauth_provider(oauth.current());
        if let Some(hook) = self.state_hook {
            state = hook(state);
        }

        let mut state_router = Router::<ApiState>::new();
        if let Some(router) = self.public_router {
            state_router = state_router.merge(router);
        }
        if let Some(router) = self.admin_router {
            state_router = state_router.merge(router);
        }
        if let Some(router) = self.rpc_router {
            state_router = state_router.merge(router);
        }
        let state_router = state_router.with_state(state.clone());

        let fallback_assets = assets.clone();
        let mut router = Router::new()
            .merge(client_router_with_state(state.clone()))
            .merge(state_router)
            .merge(oauth::router(db.clone(), oauth.clone()))
            .merge(komari_web::routes_for::<()>(assets.clone()))
            .route("/ping", any(ping))
            .fallback(move |request| {
                let assets = fallback_assets.clone();
                async move { komari_web::serve_fallback(&assets, request).await }
            });
        if let Some(stateless) = self.stateless_router {
            router = router.merge(stateless);
        }

        let auth = state.auth.clone();
        router = router
            .layer(PrivateSite::new(auth.clone()))
            .layer(AuthLayer::new(auth))
            // A real body-stream limit must wrap `AuthLayer`, which buffers
            // non-GET bodies before extractors run. `DefaultBodyLimit` alone
            // would be bypassed by that buffering.
            .layer(RequestBodyLimitLayer::new(config.body_limit_bytes))
            // CORS wraps the body limit so a 413 still carries CORS headers,
            // matching the Go middleware order.
            .layer(middleware::from_fn_with_state(
                state.clone(),
                cors_and_cache,
            ));
        if config.request_logging {
            router = router.layer(
                TraceLayer::new_for_http()
                    .make_span_with(RequestSpan)
                    .on_response(DefaultOnResponse::new().level(Level::INFO)),
            );
        }

        let background =
            BackgroundTasks::start(&state, assets.clone(), ddns.clone(), &config.background);

        info!(
            listen = %config.listen,
            database = %config.database_path.display(),
            data_dir = %config.data_dir.display(),
            theme = %assets.effective_theme(),
            background_tasks = background.task_count(),
            "Komari HTTP server assembled"
        );

        Ok(Server {
            config,
            state,
            assets,
            oauth,
            ddns,
            background,
            router,
        })
    }
}

/// A fully assembled server.
pub struct Server {
    config: ServerConfig,
    state: ApiState,
    assets: Arc<WebAssets>,
    oauth: OAuthRuntime,
    ddns: Arc<DdnsService>,
    background: BackgroundTasks,
    router: Router,
}

impl Server {
    /// Open the database, restore a pending backup and assemble the server.
    pub async fn start(config: ServerConfig) -> Result<Self> {
        ServerBuilder::new(config).build().await
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn state(&self) -> &ApiState {
        &self.state
    }

    pub fn assets(&self) -> &Arc<WebAssets> {
        &self.assets
    }

    pub fn oauth(&self) -> &OAuthRuntime {
        &self.oauth
    }

    /// Shared DDNS service for B06's admin "sync now" endpoint.
    pub fn ddns(&self) -> &Arc<DdnsService> {
        &self.ddns
    }

    pub fn background(&self) -> &BackgroundTasks {
        &self.background
    }

    /// Stop every background loop owned by the server, including the presence
    /// sweeper and report flusher owned by [`ApiState`].
    pub fn cancel_background(&self) {
        self.state.shutdown_background();
        self.background.cancel();
    }

    /// A clone of the assembled router (state already applied).
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    /// Re-read theme/site/temp-share configuration from the database.
    ///
    /// B06 should call this after updating settings for an immediate effect;
    /// the background asset task also refreshes every minute.
    pub fn refresh_assets(&self) -> Result<(), DbError> {
        refresh_web_assets(&self.assets, &self.state.db)
    }

    /// Serve until the process receives `SIGINT`/`SIGTERM`.
    pub async fn serve(self, listener: TcpListener) -> Result<()> {
        self.serve_with_shutdown(listener, shutdown_signal()).await
    }

    /// Serve with a caller-supplied shutdown future.
    pub async fn serve_with_shutdown<F>(mut self, listener: TcpListener, shutdown: F) -> Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let address = listener
            .local_addr()
            .context("failed to read listen address")?;
        info!("Starting server on {} ...", self.config.listen);
        // `Server` implements `Drop`, so the router is moved out instead of
        // destructuring `self`.
        let router = std::mem::replace(&mut self.router, Router::new());
        let serve = axum::serve(listener, router).with_graceful_shutdown(shutdown);
        let result = match tokio::time::timeout(self.config.shutdown_timeout, serve).await {
            Ok(result) => result.context("HTTP server failed"),
            Err(_) => {
                warn!(
                    timeout = ?self.config.shutdown_timeout,
                    "graceful shutdown timed out; forcing shutdown"
                );
                Ok(())
            }
        };
        let _ = self
            .state
            .db
            .logs()
            .log("", "", "server is shutting down", "info");
        self.state.shutdown_background();
        self.background
            .shutdown_with_timeout(self.config.shutdown_timeout)
            .await;
        info!(%address, "server stopped");
        result
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.state.shutdown_background();
        self.background.cancel();
    }
}

/// Open the database, restore `data/backup.zip` and migrate the schema.
///
/// A failed restore is logged and ignored, matching the Go startup path: a
/// broken old database must not prevent the operator from fixing it.
pub fn prepare_database(config: &ServerConfig) -> Result<Db> {
    std::fs::create_dir_all(&config.data_dir).with_context(|| {
        format!(
            "failed to create data directory {}",
            config.data_dir.display()
        )
    })?;
    std::fs::create_dir_all(config.data_dir.join("theme")).with_context(|| {
        format!(
            "failed to create theme directory {}",
            config.data_dir.display()
        )
    })?;

    if config.database_path.as_os_str() != ":memory:" {
        // Go snapshots the current data directory to `./backup/{ts}.zip`
        // before replacing it. Use a sibling of the data directory so the
        // archive survives the swap.
        let backup_dir = config
            .data_dir
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("backup");
        let mut options = BackupOptions::new(&config.data_dir)
            .with_database_path(&config.database_path)
            // Startup restore is deliberately more permissive than the admin
            // upload endpoint: an old database may predate the current schema
            // and may not contain the Go marker file.
            .require_markup(false)
            .allow_legacy_schema(true);
        options.pre_restore_backup_dir = Some(backup_dir.clone());
        match restore_pending_backup(&options) {
            Ok(Some(report)) => info!(
                database_bytes = report.database_bytes,
                data_entries = report.data_entries,
                pre_restore_archive = ?report.pre_restore_archive,
                "restored pending backup.zip"
            ),
            Ok(None) => {}
            Err(error) => error!(%error, "failed to restore pending backup.zip; continuing"),
        }
    }

    let db = Db::open(&config.database_path)
        .with_context(|| format!("failed to open database {}", config.database_path.display()))?;
    db.migrate().with_context(|| {
        format!(
            "failed to migrate database {}",
            config.database_path.display()
        )
    })?;

    if db
        .accounts()
        .count()
        .context("failed to count user accounts")?
        == 0
    {
        let (username, password, _) = db
            .accounts()
            .create_default_admin()
            .context("failed to create the default administrator")?;
        info!(username = %username, password = %password, "default admin account created");
    }

    Ok(db)
}

/// Start a server and listen on [`ServerConfig::listen`].
pub async fn run(config: ServerConfig) -> Result<()> {
    let listen = config.listen.clone();
    let server = Server::start(config).await?;
    let listener = TcpListener::bind(&listen)
        .await
        .with_context(|| format!("failed to bind {listen}"))?;
    server.serve(listener).await
}

async fn ping() -> &'static str {
    "pong"
}

/// Request span that logs the path but never the query string.
///
/// Agent tokens are passed as `?token=`, so logging the full URI would leak
/// credentials into access logs.
#[derive(Clone, Copy, Debug)]
struct RequestSpan;

impl<B> MakeSpan<B> for RequestSpan {
    fn make_span(&mut self, request: &http::Request<B>) -> tracing::Span {
        tracing::info_span!(
            "http",
            method = %request.method(),
            path = %request.uri().path(),
            version = ?request.version(),
        )
    }
}

/// Go's dynamic CORS middleware plus the `/api` `Cache-Control: no-store`
/// header.
async fn cors_and_cache(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let cors = state.allow_cors();

    if cors && request.method() == Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        apply_cors_headers(response.headers_mut());
        return response;
    }

    let mut response = next.run(request).await;
    if cors {
        apply_cors_headers(response.headers_mut());
    }
    if path == "/api" || path.starts_with("/api/") {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

fn apply_cors_headers(headers: &mut axum::http::HeaderMap) {
    for (name, value) in [
        (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        (
            header::ACCESS_CONTROL_ALLOW_METHODS,
            "GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS",
        ),
        (
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            "Origin, Content-Length, Content-Type, Authorization, Accept, X-CSRF-Token, X-Requested-With, Set-Cookie",
        ),
        (
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "Content-Length, Authorization, Set-Cookie",
        ),
        (header::ACCESS_CONTROL_ALLOW_CREDENTIALS, "true"),
        (header::ACCESS_CONTROL_MAX_AGE, "43200"),
    ] {
        if let Ok(value) = HeaderValue::from_str(value) {
            headers.insert(name, value);
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut signal) = signal(SignalKind::terminate()) {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
