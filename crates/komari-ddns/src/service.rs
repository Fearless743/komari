//! DDNS provider selection and client synchronization.
//!
//! This is the Rust counterpart of `utils/ddns/ddns.go`: it persists default
//! provider configurations, loads the provider selected by
//! `ddns_provider`, skips unchanged addresses, retries transient failures and
//! records every attempt in `ddns_sync_history`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use komari_config::keys::{DDNS_ENABLED_KEY, DDNS_PROVIDER_KEY};
use komari_db::models::{Client, DdnsProvider, DdnsSyncHistory};
use komari_db::{ClientPatch, Db, LocalTime};
use parking_lot::{Mutex, RwLock};
use serde_json::Value;

use crate::error::DdnsError;
use crate::factory::DdnsFactory;
use crate::provider::{
    DdnsProvider as DdnsProviderTrait, ProviderConfig, SyncContext, SyncResult, config_string,
};

/// Number of retries after the initial attempt, matching Go's `maxRetries`.
pub const DEFAULT_MAX_RETRIES: u32 = 3;

/// Base delay for exponential backoff, matching Go's `retryBaseDelay`.
pub const DEFAULT_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

/// Retry counters older than this are discarded (Go uses five minutes).
pub const DEFAULT_RETRY_RESET_WINDOW: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ClientState {
    ipv4: String,
    ipv6: String,
}

#[derive(Clone, Debug)]
struct RetryInfo {
    consecutive_failures: u32,
    last_failure: Instant,
}

/// Selects a DDNS provider and synchronizes clients against it.
pub struct DdnsService {
    db: Arc<Db>,
    factory: Arc<DdnsFactory>,
    current: RwLock<Option<Arc<dyn DdnsProviderTrait>>>,
    last_state: Mutex<HashMap<String, ClientState>>,
    retry_history: Mutex<HashMap<String, RetryInfo>>,
    max_retries: u32,
    retry_base_delay: Duration,
    retry_reset_window: Duration,
}

impl DdnsService {
    /// Create a service backed by the built-in provider registry.
    pub fn new(db: Arc<Db>) -> Self {
        Self::with_factory(db, DdnsFactory::with_builtins())
    }

    /// Create a service with a caller-supplied registry (used by tests).
    pub fn with_factory(db: Arc<Db>, factory: DdnsFactory) -> Self {
        Self {
            db,
            factory: Arc::new(factory),
            current: RwLock::new(None),
            last_state: Mutex::new(HashMap::new()),
            retry_history: Mutex::new(HashMap::new()),
            max_retries: DEFAULT_MAX_RETRIES,
            retry_base_delay: DEFAULT_RETRY_BASE_DELAY,
            retry_reset_window: DEFAULT_RETRY_RESET_WINDOW,
        }
    }

    /// Override the retry policy. A zero base delay makes tests fast.
    pub fn with_retry_policy(mut self, max_retries: u32, base_delay: Duration) -> Self {
        self.max_retries = max_retries;
        self.retry_base_delay = base_delay;
        self
    }

    /// Override how long consecutive failures are remembered.
    pub fn with_retry_reset_window(mut self, window: Duration) -> Self {
        self.retry_reset_window = window;
        self
    }

    pub fn factory(&self) -> &DdnsFactory {
        &self.factory
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Port of `ddns.CurrentProvider`.
    pub fn current_provider(&self) -> Option<Arc<dyn DdnsProviderTrait>> {
        self.current.read().clone()
    }

    /// Port of `ddns.Initialize`: persist missing default provider configs and
    /// load the provider selected by the `ddns_provider` setting.
    pub async fn initialize(&self) -> Result<(), DdnsError> {
        for (name, provider) in self.factory.get_all_providers() {
            if self.db.ddns_providers().get(&name)?.is_some() {
                continue;
            }
            let addition = serde_json::to_string(&provider.configuration())?;
            if let Err(error) = self
                .db
                .ddns_providers()
                .save(&DdnsProvider::new(name, addition))
            {
                tracing::error!("failed to save default DDNS config: {error}");
            }
        }

        let method: String = self
            .db
            .config()
            .get_as_or(DDNS_PROVIDER_KEY, "none".to_owned())?;
        if method.is_empty() || method == "none" {
            return self.load_provider("empty", "{}");
        }

        let Some(provider_config) = self.db.ddns_providers().get(&method)? else {
            return self.load_provider("empty", "{}");
        };
        if let Err(error) = self.load_provider(&method, &provider_config.addition) {
            tracing::error!("failed to load DDNS provider {method}: {error}");
            return self.load_provider("empty", "{}");
        }
        Ok(())
    }

    /// Port of `ddns.LoadProvider`.
    pub fn load_provider(&self, name: &str, addition: &str) -> Result<(), DdnsError> {
        let mut provider = self.factory.create(name)?;
        let configuration: Value = serde_json::from_str(addition)
            .map_err(|error| DdnsError::InvalidConfig(error.to_string()))?;
        provider.set_configuration(configuration)?;
        provider.init()?;
        let provider: Arc<dyn DdnsProviderTrait> = Arc::from(provider);

        let previous = self.current.write().replace(provider);
        if let Some(previous) = previous {
            if let Err(error) = previous.destroy() {
                tracing::warn!("failed to destroy previous DDNS provider: {error}");
            }
        }
        Ok(())
    }

    /// Port of `ddns.SyncAll`. Individual client failures are logged and do
    /// not stop the remaining clients.
    pub async fn sync_all(&self, clients: &[Client], triggered_by: &str, force: bool) {
        let enabled: bool = self
            .db
            .config()
            .get_as_or(DDNS_ENABLED_KEY, false)
            .unwrap_or(false);
        if !enabled && !force {
            return;
        }
        if self.current_provider().is_none() {
            return;
        }
        for client in clients {
            let _ = self.sync_client(client, triggered_by, force).await;
        }
    }

    /// Port of `ddns.SyncClient`.
    pub async fn sync_client(
        &self,
        client: &Client,
        triggered_by: &str,
        force: bool,
    ) -> Result<(), DdnsError> {
        let enabled: bool = self.db.config().get_as_or(DDNS_ENABLED_KEY, false)?;
        if !enabled && !force {
            return Ok(());
        }
        let Some(provider) = self.current_provider() else {
            return Ok(());
        };

        let ipv4 = client.ipv4.trim().to_owned();
        let ipv6 = client.ipv6.trim().to_owned();
        if client.ddns_enabled {
            if ipv4.is_empty() && ipv6.is_empty() {
                return Ok(());
            }
        } else if !force {
            return Ok(());
        }
        if ipv4.is_empty() && ipv6.is_empty() {
            return Ok(());
        }

        let new_state = ClientState {
            ipv4: ipv4.clone(),
            ipv6: ipv6.clone(),
        };
        if !force && self.last_state.lock().get(&client.uuid) == Some(&new_state) {
            return Ok(());
        }

        let mut provider_config = match self.provider_config_map() {
            Ok(config) => config,
            Err(error) => {
                tracing::warn!("failed to load DDNS provider config: {error}");
                ProviderConfig::new()
            }
        };
        provider_config = merge_client_provider_config(provider_config, client);
        let context = SyncContext {
            ipv4,
            ipv6,
            client_uuid: client.uuid.clone(),
            client_name: client.name.clone(),
            triggered_by: triggered_by.to_owned(),
            force,
            provider_config,
        };

        let result = match self.sync_with_retry(provider.as_ref(), &context).await {
            Ok(result) => result,
            Err(error) => {
                self.audit(
                    "error",
                    format!("DDNS sync failed for {}: {error}", client.uuid),
                );
                return Err(error);
            }
        };

        if !result.resolved_record_id.is_empty() && client.ddns_record_id.trim().is_empty() {
            let patch = ClientPatch {
                ddns_record_id: Some(result.resolved_record_id),
                ..ClientPatch::default()
            };
            if let Err(error) = self.db.clients().patch(&client.uuid, &patch) {
                tracing::warn!(
                    "failed to persist resolved DDNS record id for {}: {error}",
                    client.uuid
                );
            }
        }

        self.last_state
            .lock()
            .insert(client.uuid.clone(), new_state);
        let timestamp = LocalTime::now().to_rfc3339().unwrap_or_default();
        self.audit(
            "info",
            format!("DDNS synced for {} at {timestamp}", client.uuid),
        );
        Ok(())
    }

    /// Run a provider with the same exponential backoff as Go and record the
    /// final outcome in `ddns_sync_history`.
    pub async fn sync_with_retry(
        &self,
        provider: &dyn DdnsProviderTrait,
        context: &SyncContext,
    ) -> Result<SyncResult, DdnsError> {
        let mut last_error = None;
        for attempt in 0..=self.max_retries {
            if attempt > 0 {
                let delay = self.retry_delay(attempt);
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
            }
            match provider.sync(context).await {
                Ok(result) => {
                    self.reset_retry_count(&context.client_uuid);
                    self.record_sync_history(context, "success", "");
                    return Ok(result);
                }
                Err(error) => last_error = Some(error),
            }
        }

        let error =
            last_error.unwrap_or_else(|| DdnsError::Provider("ddns sync failed".to_owned()));
        self.increment_retry_count(&context.client_uuid);
        self.record_sync_history(context, "failed", &error.to_string());
        Err(DdnsError::RetryExhausted {
            retries: self.max_retries,
            source: Box::new(error),
        })
    }

    /// Number of consecutive failures remembered for a client.
    pub fn retry_count(&self, client_uuid: &str) -> u32 {
        let mut history = self.retry_history.lock();
        let Some(info) = history.get(client_uuid) else {
            return 0;
        };
        if info.last_failure.elapsed() > self.retry_reset_window {
            history.remove(client_uuid);
            return 0;
        }
        info.consecutive_failures
    }

    /// Last successfully synchronized addresses for a client.
    pub fn last_state(&self, client_uuid: &str) -> Option<(String, String)> {
        self.last_state
            .lock()
            .get(client_uuid)
            .map(|state| (state.ipv4.clone(), state.ipv6.clone()))
    }

    /// Current provider configuration merged with per-client overrides.
    pub fn provider_config_map(&self) -> Result<ProviderConfig, DdnsError> {
        let method: String = self
            .db
            .config()
            .get_as_or(DDNS_PROVIDER_KEY, "none".to_owned())?;
        if method.is_empty() || method == "none" {
            return Ok(ProviderConfig::new());
        }
        let Some(provider_config) = self.db.ddns_providers().get(&method)? else {
            return Ok(ProviderConfig::new());
        };
        match serde_json::from_str(&provider_config.addition)? {
            Value::Object(map) => Ok(map),
            _ => Err(DdnsError::InvalidConfig(
                "provider configuration must be a JSON object".to_owned(),
            )),
        }
    }

    fn retry_delay(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(10);
        self.retry_base_delay.saturating_mul(1 << exponent)
    }

    fn increment_retry_count(&self, client_uuid: &str) {
        let current = self.retry_count(client_uuid);
        self.retry_history.lock().insert(
            client_uuid.to_owned(),
            RetryInfo {
                consecutive_failures: current + 1,
                last_failure: Instant::now(),
            },
        );
    }

    fn reset_retry_count(&self, client_uuid: &str) {
        self.retry_history.lock().remove(client_uuid);
    }

    fn record_sync_history(&self, context: &SyncContext, status: &str, error: &str) {
        let history = DdnsSyncHistory {
            id: 0,
            client_uuid: context.client_uuid.clone(),
            client_name: context.client_name.clone(),
            hostname: config_string(&context.provider_config, "hostname"),
            record_type: config_string(&context.provider_config, "record_type"),
            ipv4: context.ipv4.clone(),
            ipv6: context.ipv6.clone(),
            record_id: config_string(&context.provider_config, "record_id"),
            status: status.to_owned(),
            error: error.to_owned(),
            triggered_by: context.triggered_by.clone(),
            synced_at: LocalTime::now(),
        };
        if let Err(error) = self.db.ddns().save_history(&history) {
            tracing::warn!(
                "failed to save DDNS sync history for {}: {error}",
                context.client_uuid
            );
        }
    }

    fn audit(&self, event_type: &str, message: String) {
        if let Err(error) = self.db.logs().log("", "", message, event_type) {
            tracing::warn!("failed to write DDNS audit log: {error}");
        }
    }
}

/// Merge a client's per-client DDNS settings over the provider configuration.
/// Non-empty client values win, matching Go's
/// `mergeClientProviderConfig`.
pub fn merge_client_provider_config(mut config: ProviderConfig, client: &Client) -> ProviderConfig {
    if !client.ddns_hostname.is_empty() {
        config.insert(
            "hostname".to_owned(),
            Value::String(client.ddns_hostname.clone()),
        );
    }
    if !client.ddns_record_id.is_empty() {
        config.insert(
            "record_id".to_owned(),
            Value::String(client.ddns_record_id.clone()),
        );
    }
    if !client.ddns_record_type.is_empty() {
        config.insert(
            "record_type".to_owned(),
            Value::String(client.ddns_record_type.clone()),
        );
    }
    config
}
