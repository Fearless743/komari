//! Provider registry for GeoIP backends.
//!
//! Go selects a provider with a `switch` in `geoip.InitGeoIp`; this registry
//! keeps the same keys (`mmdb`, `ip-api`, `ipinfo`, `geojs`, `empty`) while
//! making the constructors replaceable for tests.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use crate::error::GeoIpError;
use crate::provider::GeoIpProvider;

/// Creates a provider or reports why it cannot be initialized.
pub type GeoIpConstructor =
    Arc<dyn Fn() -> Result<Box<dyn GeoIpProvider>, GeoIpError> + Send + Sync>;

/// Registry of GeoIP provider constructors keyed by configuration value.
pub struct GeoIpFactory {
    registrations: RwLock<HashMap<String, GeoIpConstructor>>,
}

impl Default for GeoIpFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl GeoIpFactory {
    /// An empty registry.
    pub fn new() -> Self {
        Self {
            registrations: RwLock::new(HashMap::new()),
        }
    }

    /// A registry containing every built-in provider.
    pub fn with_builtins() -> Self {
        let factory = Self::new();
        register_builtin_providers(&factory);
        factory
    }

    /// Register a constructor under the value used by `geo_ip_provider`.
    pub fn register_provider(&self, key: &str, constructor: GeoIpConstructor) {
        self.registrations
            .write()
            .expect("geoip registry poisoned")
            .insert(key.to_owned(), constructor);
    }

    /// Create a provider by configuration key.
    pub fn create(&self, key: &str) -> Result<Box<dyn GeoIpProvider>, GeoIpError> {
        let constructor = self
            .registrations
            .read()
            .expect("geoip registry poisoned")
            .get(key)
            .cloned()
            .ok_or_else(|| GeoIpError::ProviderNotFound(key.to_owned()))?;
        constructor()
    }

    /// Registered configuration keys, sorted for deterministic output.
    pub fn provider_keys(&self) -> Vec<String> {
        let registrations = self.registrations.read().expect("geoip registry poisoned");
        let mut keys: Vec<String> = registrations.keys().cloned().collect();
        keys.sort();
        keys
    }

    pub fn contains(&self, key: &str) -> bool {
        self.registrations
            .read()
            .expect("geoip registry poisoned")
            .contains_key(key)
    }
}

/// Register the providers used by the Go `InitGeoIp` switch.
pub fn register_builtin_providers(factory: &GeoIpFactory) {
    crate::providers::empty::register(factory);
}

/// Process-wide registry with all built-in providers.
pub fn factory() -> &'static GeoIpFactory {
    static FACTORY: OnceLock<GeoIpFactory> = OnceLock::new();
    FACTORY.get_or_init(GeoIpFactory::with_builtins)
}
