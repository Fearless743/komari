//! Provider registry, mirroring Go's `utils/ddns/factory`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use serde_json::Value;

use crate::error::DdnsError;
use crate::provider::{ConfigurationItem, DdnsProvider};

/// Creates a provider with its zero-value configuration.
pub type DdnsConstructor = Arc<dyn Fn() -> Box<dyn DdnsProvider> + Send + Sync>;

struct Registration {
    constructor: DdnsConstructor,
    configuration_items: Vec<ConfigurationItem>,
}

/// Registry of DDNS provider constructors.
///
/// A process-wide registry with all built-in providers is available through
/// [`factory`]; tests can use [`DdnsFactory::new`] plus
/// [`DdnsFactory::register_provider`] to install a mock provider.
pub struct DdnsFactory {
    registrations: RwLock<HashMap<String, Registration>>,
}

impl Default for DdnsFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl DdnsFactory {
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

    /// Register a provider constructor. Its name and configuration metadata
    /// are taken from the instance it creates.
    pub fn register_provider(&self, constructor: DdnsConstructor) {
        let provider = constructor();
        let name = provider.name().to_owned();
        let configuration_items = provider.configuration_items();
        let mut registrations = self.registrations.write().expect("ddns registry poisoned");
        if registrations.contains_key(&name) {
            tracing::warn!("DDNS provider already registered: {name}");
        }
        registrations.insert(
            name,
            Registration {
                constructor,
                configuration_items,
            },
        );
    }

    /// Names of all registered providers, sorted for deterministic output.
    pub fn provider_names(&self) -> Vec<String> {
        let registrations = self.registrations.read().expect("ddns registry poisoned");
        let mut names: Vec<String> = registrations.keys().cloned().collect();
        names.sort();
        names
    }

    /// Create one fresh provider instance.
    pub fn create(&self, name: &str) -> Result<Box<dyn DdnsProvider>, DdnsError> {
        let constructor = self.get_constructor(name)?;
        Ok(constructor())
    }

    /// Port of `factory.GetConstructor`.
    pub fn get_constructor(&self, name: &str) -> Result<DdnsConstructor, DdnsError> {
        let registrations = self.registrations.read().expect("ddns registry poisoned");
        registrations
            .get(name)
            .map(|registration| Arc::clone(&registration.constructor))
            .ok_or_else(|| DdnsError::ProviderNotFound(name.to_owned()))
    }

    /// Port of `factory.GetProviderConfigs`.
    pub fn get_provider_configs(&self) -> HashMap<String, Vec<ConfigurationItem>> {
        let registrations = self.registrations.read().expect("ddns registry poisoned");
        registrations
            .iter()
            .map(|(name, registration)| (name.clone(), registration.configuration_items.clone()))
            .collect()
    }

    /// Port of `factory.GetAllDdnsProviders`: a fresh instance per provider.
    pub fn get_all_providers(&self) -> Vec<(String, Box<dyn DdnsProvider>)> {
        let constructors: Vec<(String, DdnsConstructor)> = {
            let registrations = self.registrations.read().expect("ddns registry poisoned");
            registrations
                .iter()
                .map(|(name, registration)| (name.clone(), Arc::clone(&registration.constructor)))
                .collect()
        };
        constructors
            .into_iter()
            .map(|(name, constructor)| (name, constructor()))
            .collect()
    }
}

/// Register the providers that the Go backend links through `ddns.All()`.
pub fn register_builtin_providers(factory: &DdnsFactory) {
    crate::providers::cloudflare::register(factory);
    crate::providers::empty::register(factory);
    crate::providers::webhook::register(factory);
}

/// Process-wide registry with all built-in providers.
pub fn factory() -> &'static DdnsFactory {
    static FACTORY: OnceLock<DdnsFactory> = OnceLock::new();
    FACTORY.get_or_init(DdnsFactory::with_builtins)
}

/// Pretty-print a provider configuration for diagnostics and storage.
pub fn configuration_to_json(provider: &dyn DdnsProvider) -> Result<String, DdnsError> {
    let configuration: Value = provider.configuration();
    serde_json::to_string(&configuration)
        .map_err(|error| DdnsError::InvalidConfig(error.to_string()))
}
