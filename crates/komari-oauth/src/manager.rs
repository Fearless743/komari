//! Process-wide provider manager (Go's `utils/oauth` package).
//!
//! The manager keeps the currently active provider and mirrors Go's
//! `Initialize`: it seeds a default configuration row for every built-in
//! provider, reads `o_auth_provider` from the key/value config table and falls
//! back to GitHub when the configured provider does not exist.

use std::sync::{Arc, OnceLock};

use komari_db::Db;
use komari_db::models::Provider;
use parking_lot::RwLock;

use crate::error::Result;
use crate::factory::{ProviderRegistry, default_config, default_http_client};
use crate::provider::OAuthProvider;

/// Configuration key used by Go's `config.OAuthProviderKey`.
pub const OAUTH_PROVIDER_KEY: &str = "o_auth_provider";
/// Configuration key used by Go's `config.OAuthEnabledKey`.
pub const OAUTH_ENABLED_KEY: &str = "o_auth_enabled";
/// Provider used when no configuration exists (Go's default).
pub const DEFAULT_PROVIDER: &str = "github";

/// Owns the active provider instance.
pub struct OAuthManager {
    registry: &'static ProviderRegistry,
    current: RwLock<Option<Arc<dyn OAuthProvider>>>,
}

impl Default for OAuthManager {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthManager {
    pub fn new() -> Self {
        Self {
            registry: crate::factory::builtin_registry(),
            current: RwLock::new(None),
        }
    }

    pub fn with_registry(registry: &'static ProviderRegistry) -> Self {
        Self {
            registry,
            current: RwLock::new(None),
        }
    }

    /// The active provider, if one has been loaded.
    pub fn current(&self) -> Option<Arc<dyn OAuthProvider>> {
        self.current.read().clone()
    }

    /// Name of the active provider.
    pub fn current_name(&self) -> Option<&'static str> {
        self.current.read().as_ref().map(|provider| provider.name())
    }

    /// Load a provider from JSON configuration, replacing the active one.
    pub async fn load_provider(
        &self,
        name: &str,
        config_json: &str,
    ) -> Result<Arc<dyn OAuthProvider>> {
        let value = if config_json.trim().is_empty() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(config_json)
                .map_err(|error| crate::OAuthError::InvalidConfig(error.to_string()))?
        };
        let provider = self
            .registry
            .create_and_init_with_client(name, value, default_http_client())
            .await?;

        let previous = self.current.write().replace(Arc::clone(&provider));
        if let Some(previous) = previous {
            if let Err(error) = previous.destroy().await {
                tracing::warn!(
                    provider = previous.name(),
                    error = %crate::error::redacted_error(&error, &[]),
                    "failed to destroy previous OAuth provider"
                );
            }
        }
        Ok(provider)
    }

    /// Seed default rows and load the configured provider (Go `Initialize`).
    pub async fn initialize_from_db(&self, db: &Db) -> Result<()> {
        for meta in self.registry.metadata() {
            if db.oauth().get(meta.name)?.is_none() {
                let addition = serde_json::to_string(&default_config(meta.name))?;
                db.oauth().save(&Provider::new(meta.name, addition))?;
            }
        }

        let configured = db
            .config()
            .get_as::<String>(OAUTH_PROVIDER_KEY)?
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_PROVIDER.to_owned());

        if configured == "none" {
            return self.load_provider(DEFAULT_PROVIDER, "{}").await.map(|_| ());
        }

        match db.oauth().get(&configured)? {
            Some(provider) => self
                .load_provider(&provider.name, &provider.addition)
                .await
                .map(|_| ()),
            None => {
                // Go falls back to GitHub when the configured row is missing.
                self.load_provider(DEFAULT_PROVIDER, "{}").await.map(|_| ())
            }
        }
    }

    /// Drop the active provider and release its cached state.
    pub async fn clear(&self) {
        let previous = self.current.write().take();
        if let Some(provider) = previous {
            let _ = provider.destroy().await;
        }
    }
}

/// Process-wide manager used by the API layer.
pub fn global() -> &'static OAuthManager {
    static MANAGER: OnceLock<OAuthManager> = OnceLock::new();
    MANAGER.get_or_init(OAuthManager::new)
}

/// Convenience wrapper matching Go's `oauth.LoadProvider`.
pub async fn load_provider_global(name: &str, config_json: &str) -> Result<Arc<dyn OAuthProvider>> {
    global().load_provider(name, config_json).await
}

/// Convenience wrapper matching Go's `oauth.CurrentProvider`.
pub fn current_provider() -> Option<Arc<dyn OAuthProvider>> {
    global().current()
}
