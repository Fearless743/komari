//! Shared application state for the API routers.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::FromRef;
use komari_auth::AuthState;
use komari_config::keys;
use komari_db::{Db, DbError};
use komari_ddns::DdnsService;
use komari_geoip::GeoIpService;
use komari_jobs::CancelToken;
use komari_oauth::OAuthProvider;
use komari_web::{WebAssets, WebConfig};

use crate::cache::ReportCache;
use crate::clock::{Clock, SystemClock};
use crate::presence::PresenceTracker;
use crate::ws::WsHub;

/// Timeouts and limits used by the agent routes.
#[derive(Clone, Debug)]
pub struct ApiConfig {
    /// Force cross-origin WebSockets to be accepted. The persisted
    /// `allow_cors` configuration is still checked by [`ApiState::allow_cors`].
    pub allow_cors: bool,
    /// Skip the `Origin`/`Host` comparison (`KOMARI_WS_DISABLE_ORIGIN`).
    pub ws_disable_origin: bool,
    /// TTL refreshed by every HTTP report; matches Go's `readWait`.
    pub presence_ttl: Duration,
    /// WebSocket read timeout; Go also uses 11 seconds.
    pub socket_read_timeout: Duration,
    /// How often the background presence sweeper runs.
    pub presence_sweep_interval: Duration,
    /// Retention window for buffered reports (Go's 1-minute cache).
    pub report_retention: Duration,
    /// How often buffered reports are aggregated into `records`.
    pub report_flush_interval: Duration,
    /// Top fraction used by `AverageReport` (Go passes 0.3).
    pub report_top_percentage: f64,
    /// Delay between MJPEG frames (Go's `refreshInterval`, 2 seconds).
    pub mjpeg_frame_interval: Duration,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            allow_cors: false,
            ws_disable_origin: false,
            presence_ttl: Duration::from_secs(11),
            socket_read_timeout: Duration::from_secs(11),
            presence_sweep_interval: Duration::from_secs(1),
            report_retention: Duration::from_secs(60),
            report_flush_interval: Duration::from_secs(60),
            report_top_percentage: 0.3,
            mjpeg_frame_interval: Duration::from_secs(2),
        }
    }
}

impl ApiConfig {
    /// Load startup defaults. `allow_cors` is deliberately *not* snapshotted:
    /// the Go backend reads the persisted value on every upgrade, so
    /// [`ApiState::allow_cors`] consults the database each time.
    pub fn from_db(_db: &Db) -> Self {
        Self::default()
    }
}

/// State shared by every `komari-api` route.
#[derive(Clone)]
pub struct ApiState {
    pub db: Arc<Db>,
    pub auth: AuthState,
    pub hub: Arc<WsHub>,
    pub presence: Arc<PresenceTracker>,
    pub report_cache: Arc<ReportCache>,
    /// Optional GeoIP service. It is not initialized automatically; the
    /// server assembly can call `initialize()` and install it with
    /// [`ApiState::with_geoip_service`].
    pub geoip: Option<Arc<GeoIpService>>,
    /// Embedded/static web assets. Admin theme routes use the same handle as
    /// the static router so a switch takes effect immediately.
    pub web: Arc<WebAssets>,
    /// DDNS service used by the admin sync endpoint. The server assembly can
    /// install an initialized instance with [`ApiState::with_ddns_service`].
    pub ddns: Arc<DdnsService>,
    /// Active OAuth provider. It is `None` until the server assembly loads the
    /// configured provider (see `komari_api::public::initialize_oauth`).
    oauth_provider: Arc<RwLock<Option<Arc<dyn OAuthProvider>>>>,
    config: Arc<ApiConfig>,
    /// Cancels the presence sweeper and report flusher started by
    /// [`ApiState::with_clock`].
    background_cancel: CancelToken,
}

impl FromRef<ApiState> for AuthState {
    fn from_ref(state: &ApiState) -> Self {
        state.auth.clone()
    }
}

impl ApiState {
    /// Build state with the system clock and persisted/startup configuration.
    pub fn new(db: Db) -> Self {
        let config = ApiConfig::from_db(&db);
        Self::with_config(db, config)
    }

    /// Build state with an explicit configuration; convenient for tests.
    pub fn with_config(db: Db, config: ApiConfig) -> Self {
        Self::with_clock(db, config, Arc::new(SystemClock))
    }

    /// Build state with an injected clock.
    pub fn with_clock(db: Db, config: ApiConfig, clock: Arc<dyn Clock>) -> Self {
        let db = Arc::new(db);
        let hub = Arc::new(WsHub::new(clock.clone()));
        let presence = Arc::new(PresenceTracker::new(
            clock.clone(),
            config.presence_ttl,
            Arc::clone(&hub),
        ));
        let report_cache = Arc::new(ReportCache::new(
            clock,
            config.report_retention,
            config.report_top_percentage,
        ));
        let auth = AuthState::new(Arc::clone(&db));
        let geoip = Some(Arc::new(GeoIpService::new().with_database(Arc::clone(&db))));
        let web = Arc::new(WebAssets::new(WebConfig::default()));
        let ddns = Arc::new(DdnsService::new(Arc::clone(&db)));
        let state = Self {
            db,
            auth,
            hub,
            presence,
            report_cache,
            geoip,
            web,
            ddns,
            oauth_provider: Arc::new(RwLock::new(None)),
            config: Arc::new(config),
            background_cancel: CancelToken::new(),
        };
        state.spawn_presence_sweeper();
        state.spawn_report_flusher();
        state
    }

    /// Stop the presence sweeper and report flusher.
    ///
    /// The server assembly calls this during graceful shutdown. The tasks are
    /// also stopped when the Tokio runtime is dropped.
    pub fn shutdown_background(&self) {
        self.background_cancel.cancel();
    }

    /// Whether [`ApiState::shutdown_background`] has been called.
    pub fn background_cancelled(&self) -> bool {
        self.background_cancel.is_cancelled()
    }

    pub fn config(&self) -> &ApiConfig {
        &self.config
    }

    /// Install a GeoIP service (already initialized by the server assembly).
    pub fn with_geoip_service(mut self, service: Arc<GeoIpService>) -> Self {
        self.geoip = Some(service);
        self
    }

    /// Share the static router's [`WebAssets`] with the admin theme routes.
    pub fn with_web_assets(mut self, assets: Arc<WebAssets>) -> Self {
        self.web = assets;
        self
    }

    /// Install an initialized DDNS service.
    pub fn with_ddns_service(mut self, service: Arc<DdnsService>) -> Self {
        self.ddns = service;
        self
    }

    /// Directory that holds `komari.db`, installed themes and the favicon.
    pub fn data_dir(&self) -> PathBuf {
        self.web.config().data_dir.clone()
    }

    /// The active OAuth provider, if the server assembly installed one.
    pub fn oauth_provider(&self) -> Option<Arc<dyn OAuthProvider>> {
        self.oauth_provider
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Install or replace the active OAuth provider.
    pub fn set_oauth_provider(&self, provider: Option<Arc<dyn OAuthProvider>>) {
        *self
            .oauth_provider
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = provider;
    }

    /// Builder variant of [`ApiState::set_oauth_provider`].
    pub fn with_oauth_provider(self, provider: Arc<dyn OAuthProvider>) -> Self {
        self.set_oauth_provider(Some(provider));
        self
    }
    fn spawn_presence_sweeper(&self) {
        let interval = self.config.presence_sweep_interval;
        if interval.is_zero() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                "ApiState constructed outside a Tokio runtime; presence sweeper is disabled"
            );
            return;
        };
        let presence = Arc::clone(&self.presence);
        let cancel = self.background_cancel.clone();
        handle.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        presence.expire_due();
                    }
                }
            }
        });
    }

    fn spawn_report_flusher(&self) {
        let interval = self.config.report_flush_interval;
        if interval.is_zero() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                "ApiState constructed outside a Tokio runtime; report flusher is disabled"
            );
            return;
        };
        let cache = Arc::clone(&self.report_cache);
        let db = Arc::clone(&self.db);
        let cancel = self.background_cancel.clone();
        handle.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => break,
                    _ = ticker.tick() => match cache.flush(&db) {
                        Ok(summary) => {
                            tracing::debug!(?summary, "flushed client reports");
                        }
                        Err((summary, error)) => {
                            tracing::error!(?summary, %error, "failed to flush client reports");
                        }
                    },
                }
            }
        });
    }

    /// Aggregate and persist buffered reports immediately.
    pub fn flush_reports(
        &self,
    ) -> Result<crate::cache::FlushSummary, (crate::cache::FlushSummary, String)> {
        self.report_cache.flush(&self.db)
    }

    /// Read the auto-discovery key from the persisted configuration.
    ///
    /// Missing keys are persisted as the empty string, exactly like
    /// `config.GetAs[string](config.AutoDiscoveryKeyKey, "")`.
    pub fn auto_discovery_key(&self) -> Result<String, DbError> {
        self.db
            .config()
            .get_as_or(keys::AUTO_DISCOVERY_KEY_KEY, String::new())
    }

    /// Whether cross-origin WebSockets are allowed.
    ///
    /// The value is read from the database on each upgrade so admin updates
    /// take effect without restarting the API.
    pub fn allow_cors(&self) -> bool {
        match self.db.config().get_as::<bool>(keys::ALLOW_CORS_KEY) {
            Ok(Some(value)) => value,
            // Only an absent key falls back to the explicit startup value; a
            // persisted `false` always wins.
            Ok(None) => self.config.allow_cors,
            Err(error) => {
                // Go `GetAs` returns the zero value (false) on decode errors.
                tracing::warn!(%error, "failed to read allow_cors; defaulting to false");
                false
            }
        }
    }

    /// Whether the `Origin` check is disabled.
    ///
    /// Go accepts only a case-insensitive `"true"` in
    /// `KOMARI_WS_DISABLE_ORIGIN`; the environment is read on each upgrade so
    /// changes take effect without rebuilding the state.
    pub fn ws_disable_origin(&self) -> bool {
        self.config.ws_disable_origin
            || std::env::var(keys::ENV_WS_DISABLE_ORIGIN)
                .map(|value| parse_ws_disable_origin(&value))
                .unwrap_or(false)
    }
}

fn parse_ws_disable_origin(value: &str) -> bool {
    // Go compares against "true" with `strings.EqualFold` without trimming;
    // values like "1", "yes" and " true " do *not* disable the check.
    value.eq_ignore_ascii_case("true")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_disable_origin_only_accepts_true() {
        assert!(parse_ws_disable_origin("true"));
        assert!(parse_ws_disable_origin("TRUE"));
        assert!(!parse_ws_disable_origin(" True "));
        assert!(!parse_ws_disable_origin("1"));
        assert!(!parse_ws_disable_origin("yes"));
        assert!(!parse_ws_disable_origin("on"));
        assert!(!parse_ws_disable_origin(""));
    }
}
