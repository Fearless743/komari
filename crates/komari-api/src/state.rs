//! Shared application state for the API routers.

use std::sync::Arc;
use std::time::Duration;

use komari_config::keys;
use komari_db::{Db, DbError};

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
    /// Maximum accepted JSON body for report/basic-info requests.
    pub max_report_body: usize,
    /// How often the background presence sweeper runs.
    pub presence_sweep_interval: Duration,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            allow_cors: false,
            ws_disable_origin: false,
            presence_ttl: Duration::from_secs(11),
            socket_read_timeout: Duration::from_secs(11),
            max_report_body: 1024 * 1024,
            presence_sweep_interval: Duration::from_secs(1),
        }
    }
}

impl ApiConfig {
    /// Load the persisted/startup configuration used by this task.
    pub fn from_db(db: &Db) -> Self {
        let allow_cors = db
            .config()
            .get_as_or::<bool>(keys::ALLOW_CORS_KEY, false)
            .unwrap_or(false);
        let ws_disable_origin = std::env::var(keys::ENV_WS_DISABLE_ORIGIN)
            .map(|value| truthy(&value))
            .unwrap_or(false);
        Self {
            allow_cors,
            ws_disable_origin,
            ..Self::default()
        }
    }
}

fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// State shared by every `komari-api` route.
#[derive(Clone)]
pub struct ApiState {
    pub db: Arc<Db>,
    pub hub: Arc<WsHub>,
    pub presence: Arc<PresenceTracker>,
    config: Arc<ApiConfig>,
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
            clock,
            config.presence_ttl,
            Arc::clone(&hub),
        ));
        let state = Self {
            db,
            hub,
            presence,
            config: Arc::new(config),
        };
        state.spawn_presence_sweeper();
        state
    }

    pub fn config(&self) -> &ApiConfig {
        &self.config
    }

    fn spawn_presence_sweeper(&self) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let presence = Arc::clone(&self.presence);
        let interval = self.config.presence_sweep_interval;
        if interval.is_zero() {
            return;
        }
        handle.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                presence.expire_due();
            }
        });
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
        if self.config.allow_cors {
            return true;
        }
        self.db
            .config()
            .get_as_or::<bool>(keys::ALLOW_CORS_KEY, false)
            .unwrap_or(false)
    }

    /// Whether the `Origin` check is disabled.
    pub fn ws_disable_origin(&self) -> bool {
        self.config.ws_disable_origin
    }
}
