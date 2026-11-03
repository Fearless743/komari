//! HTTP server assembly.
//!
//! This module ports the wiring in `cmd/server.go`: it opens and migrates the
//! database, restores a pending `data/backup.zip`, initializes GeoIP, DDNS,
//! notifications and OAuth, mounts the agent/browser routes, the OAuth routes
//! and the static web assets, then starts the background loops.
//!
//! # B04/B06 integration
//!
//! B04 (`public`) and B06 (`admin`/`rpc`) are merged by default. Their
//! state-applied routers can also be supplied explicitly:
//!
//! ```no_run
//! # use axum::Router;
//! # use komari_api::server::ServerBuilder;
//! # fn wire(builder: ServerBuilder, public: Router, admin: Router, rpc: Router) -> ServerBuilder {
//! builder
//!     .merge_public(public)
//!     .merge_admin(admin)
//!     .merge_rpc(rpc)
//! # }
//! ```
//!
//! The routers must already have their state applied (for example
//! `komari_api::public_router_with_state(state)`). A router without state can
//! be attached with [`ServerBuilder::merge_stateless`].

mod assets;
mod background;
pub mod oauth;

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
use komari_auth::{AuthLayer, BufferedBodyLimit, PrivateSite, payload_too_large_response};
use komari_config::Config;
use komari_db::{BackupOptions, Db, DbError, restore_pending_backup};
use komari_ddns::DdnsService;
use komari_geoip::GeoIpService;
use komari_web::WebAssets;
use tokio::net::TcpListener;
use tower_http::trace::{DefaultOnResponse, MakeSpan, TraceLayer};
use tracing::{Level, error, info, warn};

use crate::client_router_with_state;
use crate::state::{ApiConfig, ApiState};

/// Default listen address, matching the Go backend.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:25774";
/// Default SQLite database path, matching the Go backend.
pub const DEFAULT_DB_FILE: &str = "data/komari.db";
/// Default normal request-body limit (16 MiB).
///
/// The limit is applied before authentication, so oversized chunked bodies are
/// rejected with 413 rather than being buffered. Admin backup uploads use
/// [`ServerConfig::backup_body_limit_bytes`] instead.
pub const DEFAULT_BODY_LIMIT: usize = 16 * 1024 * 1024;
/// Default limit for admin backup uploads (2 GiB).
pub const DEFAULT_BACKUP_BODY_LIMIT: usize = 2 * 1024 * 1024 * 1024;

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
    /// Limit for `/api/admin/upload/backup`; backups may exceed the normal
    /// request limit.
    pub backup_body_limit_bytes: usize,
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
            backup_body_limit_bytes: DEFAULT_BACKUP_BODY_LIMIT,
            request_logging: true,
            shutdown_timeout: Duration::from_secs(5),
        }
    }
}

impl ServerConfig {
    /// Convert the CLI/startup configuration.
    pub fn from_app_config(config: &Config) -> Self {
        let database_path = PathBuf::from(&config.db.file);
        // The Go backend always uses `./data` for themes, favicon and
        // `backup.zip`, regardless of `--database`.
        let data_dir = PathBuf::from("data");
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
            body_limit_bytes: config.body_limit.unwrap_or(DEFAULT_BODY_LIMIT),
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
    public_router: Option<Router>,
    admin_router: Option<Router>,
    rpc_router: Option<Router>,
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

    /// Attach the B04 public API router.
    ///
    /// Use [`crate::public_router_with_state`] (or any router with its state
    /// already applied). The public router owns `/ping`, `/api/oauth` and
    /// `/api/oauth_callback`; B09's built-in copies are not mounted.
    pub fn merge_public(mut self, router: Router) -> Self {
        self.public_router = Some(router);
        self
    }

    /// Attach the B06 admin API router.
    ///
    /// Use [`crate::admin_router_with_state`] or another state-applied router.
    pub fn merge_admin(mut self, router: Router) -> Self {
        self.admin_router = Some(router);
        self
    }

    /// Attach the B04/B06 JSON-RPC router.
    ///
    /// Use [`crate::rpc_router_with_state`] or another state-applied router.
    pub fn merge_rpc(mut self, router: Router) -> Self {
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
            if oauth.has_override() {
                state.set_oauth_provider(oauth.current());
            } else if let Err(error) = crate::initialize_oauth(&state).await {
                warn!(%error, "failed to initialize the OAuth provider");
            }
            if let Err(error) = komari_notify::initialize_defaults(&db) {
                warn!(%error, "failed to initialize notification providers");
            }
        } else {
            state.set_oauth_provider(oauth.current());
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
        if let Some(hook) = self.state_hook {
            state = hook(state);
        }

        let public_router = self
            .public_router
            .unwrap_or_else(|| crate::public_router_with_state(state.clone()));
        let admin_router = self
            .admin_router
            .unwrap_or_else(|| crate::admin_router_with_state(state.clone()));
        let rpc_router = self
            .rpc_router
            .unwrap_or_else(|| crate::rpc_router_with_state(state.clone()));

        let fallback_assets = assets.clone();
        let fallback_db = db.clone();
        let mut router = Router::new()
            .merge(client_router_with_state(state.clone()))
            .merge(public_router)
            .merge(admin_router)
            .merge(rpc_router)
            .merge(komari_web::routes_for::<()>(assets.clone()))
            .fallback(move |request| {
                let assets = fallback_assets.clone();
                let db = fallback_db.clone();
                async move {
                    // Go reads site/theme/temp-share configuration on every
                    // request. Refresh the snapshot so B06 settings changes are
                    // visible immediately instead of after the 60s task.
                    if let Err(error) = refresh_web_assets(&assets, &db) {
                        warn!(%error, "failed to refresh web assets for request");
                    }
                    komari_web::serve_fallback(&assets, request).await
                }
            });
        if let Some(stateless) = self.stateless_router {
            router = router.merge(stateless);
        }

        let auth = state.auth.clone();
        router = router
            .layer(PrivateSite::new(auth.clone()))
            .layer(AuthLayer::new(auth))
            // The limit is inserted as a request extension before `AuthLayer`
            // buffers non-GET bodies, so chunked oversized bodies also become
            // 413 instead of being silently treated as unauthenticated.
            .layer(middleware::from_fn_with_state(
                BodyLimits {
                    normal: config.body_limit_bytes,
                    backup: config.backup_body_limit_bytes,
                },
                body_limit,
            ))
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
    /// the SPA fallback also refreshes on every request.
    pub fn refresh_assets(&self) -> Result<(), DbError> {
        refresh_web_assets(&self.assets, &self.state.db)
    }

    /// Reload GeoIP, notification, DDNS, OAuth and web-asset configuration.
    ///
    /// Go subscribes to `config` changes and reloads these services. B06 can
    /// call this after a settings update; failures are logged and do not abort
    /// the server.
    pub async fn reload_services(&self) {
        if let Some(geoip) = self.state.geoip.as_ref() {
            if let Err(error) = geoip.initialize().await {
                warn!(%error, "failed to reload GeoIP");
            }
        }
        if let Err(error) = komari_notify::initialize_defaults(&self.state.db) {
            warn!(%error, "failed to reload notification providers");
        }
        if let Err(error) = self.ddns.initialize().await {
            warn!(%error, "failed to reload DDNS");
        }
        if self.oauth.has_override() {
            self.state.set_oauth_provider(self.oauth.current());
        } else if let Err(error) = crate::initialize_oauth(&self.state).await {
            warn!(%error, "failed to reload the OAuth provider");
        }
        if let Err(error) = self.refresh_assets() {
            warn!(%error, "failed to reload web assets");
        }
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
        // `import_backup` swaps the whole data directory, so the archive's
        // database always lands at `<data_dir>/komari.db`. Restore through that
        // path and move it to the configured `--database` name afterwards.
        let standard_database = config.data_dir.join("komari.db");
        let restore_database = if config.database_path == standard_database {
            config.database_path.clone()
        } else {
            standard_database.clone()
        };
        let mut options = BackupOptions::new(&config.data_dir)
            .with_database_path(&restore_database)
            // Startup restore is deliberately more permissive than the admin
            // upload endpoint: an old database may predate the current schema
            // and may not contain the Go marker file.
            .require_markup(false)
            .allow_legacy_schema(true);
        options.pre_restore_backup_dir = Some(backup_dir.clone());
        match restore_pending_backup(&options) {
            Ok(Some(report)) => {
                info!(
                    database_bytes = report.database_bytes,
                    data_entries = report.data_entries,
                    pre_restore_archive = ?report.pre_restore_archive,
                    "restored pending backup.zip"
                );
                if restore_database != config.database_path && restore_database.is_file() {
                    move_database(&restore_database, &config.database_path)
                        .context("failed to move the restored database to the configured path")?;
                }
            }
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

/// Move a restored database to a custom `--database` path.
///
/// `rename(2)` is atomic but fails across filesystems, so fall back to a copy.
fn move_database(source: &std::path::Path, target: &std::path::Path) -> Result<()> {
    if let Some(parent) = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create database directory {}", parent.display()))?;
    }
    match std::fs::rename(source, target) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(source, target)
                .with_context(|| format!("failed to copy database to {}", target.display()))?;
            std::fs::remove_file(source)
                .with_context(|| format!("failed to remove {}", source.display()))?;
            Ok(())
        }
    }
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

#[derive(Clone, Copy, Debug)]
struct BodyLimits {
    normal: usize,
    backup: usize,
}

fn is_backup_upload(path: &str) -> bool {
    path.starts_with("/api/admin/upload/backup")
}

async fn body_limit(
    State(limits): State<BodyLimits>,
    mut request: Request,
    next: Next,
) -> Response {
    let limit = if is_backup_upload(request.uri().path()) {
        limits.backup
    } else {
        limits.normal
    };
    if let Some(length) = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
    {
        if length > limit {
            return payload_too_large_response();
        }
    }
    request.extensions_mut().insert(BufferedBodyLimit(limit));
    next.run(request).await
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

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use super::*;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::fmt::format::FmtSpan;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Buffer {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn request_span_logs_the_path_but_not_query_tokens() {
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_ansi(false)
            .with_span_events(FmtSpan::NEW)
            .with_max_level(Level::INFO)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let request = http::Request::builder()
                .uri("/api/clients?token=super-secret")
                .body(())
                .expect("request");
            let span = RequestSpan.make_span(&request);
            let _guard = span.enter();
            tracing::info!("inside request");
        });
        let output = String::from_utf8(
            buffer
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
        )
        .expect("utf8");
        assert!(output.contains("path=/api/clients"), "{output}");
        assert!(!output.contains("super-secret"), "{output}");
    }
}
