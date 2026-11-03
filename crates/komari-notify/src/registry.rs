//! Provider factory and loader (Go `utils/messageSender/factory|loader.go`).
//!
//! The registry keeps three things per provider, all of which Go derives via
//! reflection:
//! * the constructor,
//! * the default configuration object,
//! * the list of configuration fields exposed to the admin API.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde_json::{Map, Value};

use crate::error::{NotifyError, Result};
use crate::provider::{ConfigField, MessageSender};
use crate::providers;

/// A registered provider and its metadata.
pub struct ProviderRegistration {
    name: &'static str,
    default_config: Value,
    fields: Vec<ConfigField>,
    sensitive_fields: &'static [&'static str],
    constructor: fn(Value) -> Result<Box<dyn MessageSender>>,
}

impl ProviderRegistration {
    pub fn new(
        name: &'static str,
        default_config: Value,
        fields: Vec<ConfigField>,
        constructor: fn(Value) -> Result<Box<dyn MessageSender>>,
    ) -> Self {
        Self {
            name,
            default_config,
            fields,
            sensitive_fields: &[],
            constructor,
        }
    }

    /// Declare the configuration keys that must be removed from API responses.
    pub fn sensitive(mut self, fields: &'static [&'static str]) -> Self {
        self.sensitive_fields = fields;
        self
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn default_config(&self) -> &Value {
        &self.default_config
    }

    pub fn fields(&self) -> &[ConfigField] {
        &self.fields
    }

    pub fn sensitive_fields(&self) -> &'static [&'static str] {
        self.sensitive_fields
    }

    pub fn create(&self, addition: &str) -> Result<Box<dyn MessageSender>> {
        if addition.trim().is_empty() {
            // Go `json.Unmarshal("")` returns "unexpected end of JSON input".
            return Err(NotifyError::InvalidConfig(format!(
                "failed to load config for provider {}: unexpected end of JSON input",
                self.name
            )));
        }

        let value: Value = serde_json::from_str(addition).map_err(|error| {
            NotifyError::InvalidConfig(format!(
                "failed to load config for provider {}: {error}",
                self.name
            ))
        })?;
        (self.constructor)(normalize_addition(value))
    }
}

/// Registry of message sender providers.
pub struct ProviderRegistry {
    providers: HashMap<&'static str, ProviderRegistration>,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

/// Match Go's `json.Unmarshal` into a struct: a top-level `null` leaves every
/// field at its zero value, and a `null` field value does the same. Removing
/// those keys lets serde defaults produce the Go zero value.
fn normalize_addition(value: Value) -> Value {
    match value {
        Value::Null => Value::Object(Map::new()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, value)| !value.is_null())
                .collect(),
        ),
        other => other,
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    /// Register every provider Go imports from `all.go`.
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        registry.register(
            ProviderRegistration::new(
                "telegram",
                providers::telegram::default_config(),
                providers::telegram::fields(),
                providers::telegram::constructor,
            )
            .sensitive(providers::telegram::SENSITIVE_FIELDS),
        );
        registry.register(
            ProviderRegistration::new(
                "bark",
                providers::bark::default_config(),
                providers::bark::fields(),
                providers::bark::constructor,
            )
            .sensitive(providers::bark::SENSITIVE_FIELDS),
        );
        registry.register(
            ProviderRegistration::new(
                "webhook",
                providers::webhook::default_config(),
                providers::webhook::fields(),
                providers::webhook::constructor,
            )
            .sensitive(providers::webhook::SENSITIVE_FIELDS),
        );
        registry.register(
            ProviderRegistration::new(
                "email",
                providers::email::default_config(),
                providers::email::fields(),
                providers::email::constructor,
            )
            .sensitive(providers::email::SENSITIVE_FIELDS),
        );
        registry.register(
            ProviderRegistration::new(
                "Server酱³",
                providers::serverchan3::default_config(),
                providers::serverchan3::fields(),
                providers::serverchan3::constructor,
            )
            .sensitive(providers::serverchan3::SENSITIVE_FIELDS),
        );
        registry.register(
            ProviderRegistration::new(
                "Server酱Turbo",
                providers::serverchanturbo::default_config(),
                providers::serverchanturbo::fields(),
                providers::serverchanturbo::constructor,
            )
            .sensitive(providers::serverchanturbo::SENSITIVE_FIELDS),
        );
        registry.register(
            ProviderRegistration::new(
                "empty",
                providers::empty::default_config(),
                providers::empty::fields(),
                providers::empty::constructor,
            )
            .sensitive(providers::empty::SENSITIVE_FIELDS),
        );
        registry.register(
            ProviderRegistration::new(
                "Javascript",
                providers::javascript::default_config(),
                providers::javascript::fields(),
                providers::javascript::constructor,
            )
            .sensitive(providers::javascript::SENSITIVE_FIELDS),
        );
        registry
    }

    pub fn register(&mut self, registration: ProviderRegistration) {
        self.providers.insert(registration.name(), registration);
    }

    pub fn get(&self, name: &str) -> Option<&ProviderRegistration> {
        self.providers.get(name)
    }

    /// Sensitive keys for a provider, or `None` when it is unknown.
    pub fn sensitive_fields(&self, name: &str) -> Option<&'static [&'static str]> {
        self.get(name)
            .map(|registration| registration.sensitive_fields())
    }

    pub fn names(&self) -> Vec<&'static str> {
        let mut names = self.providers.keys().copied().collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    /// Map of provider name to its configuration fields, matching
    /// `factory.GetSenderConfigs()`.
    pub fn config_fields(&self) -> HashMap<&'static str, Vec<ConfigField>> {
        self.providers
            .iter()
            .map(|(name, registration)| (*name, registration.fields().to_vec()))
            .collect()
    }

    /// Default configuration object for a provider, matching
    /// `GetProviderConfiguration`.
    pub fn get_configuration(&self, name: &str) -> Result<Value> {
        self.get(name)
            .map(|registration| registration.default_config().clone())
            .ok_or_else(|| NotifyError::ProviderNotFound(name.to_owned()))
    }

    /// Construct a provider from a JSON `addition` blob and run `Init`.
    pub async fn load(&self, name: &str, addition: &str) -> Result<Box<dyn MessageSender>> {
        let registration = self
            .get(name)
            .ok_or_else(|| NotifyError::ProviderNotFound(name.to_owned()))?;
        let provider = registration.create(addition)?;
        provider.init().await?;
        Ok(provider)
    }
}

/// Process-wide registry containing the Go built-ins.
pub fn registry() -> &'static ProviderRegistry {
    static REGISTRY: LazyLock<ProviderRegistry> = LazyLock::new(ProviderRegistry::builtin);
    &REGISTRY
}

/// Load a provider by name and JSON configuration (`LoadProvider`).
pub async fn load_provider(name: &str, addition: &str) -> Result<Box<dyn MessageSender>> {
    registry().load(name, addition).await
}

/// Default configuration for a provider (`GetProviderConfiguration`).
pub fn get_provider_configuration(name: &str) -> Result<Value> {
    registry().get_configuration(name)
}

/// Save default configurations for providers missing a row, matching
/// `messageSender.Initialize`.
pub fn initialize_defaults(db: &komari_db::Db) -> Result<usize> {
    let providers = db.message_senders();
    let mut inserted = 0;
    for name in registry().names() {
        if providers.get(name)?.is_some() {
            continue;
        }
        let config = registry()
            .get_configuration(name)
            .unwrap_or_else(|_| Value::Object(Map::new()));
        let addition = serde_json::to_string(&config)?;
        providers.save(&komari_db::models::MessageSenderProvider::new(
            name, addition,
        ))?;
        inserted += 1;
    }
    Ok(inserted)
}

/// Load the provider selected by `notification_method` from the database.
///
/// Mirrors `messageSender.Initialize`: `none`/empty or a missing configuration
/// falls back to the `empty` provider.
pub async fn load_provider_from_db(db: &komari_db::Db) -> Result<Box<dyn MessageSender>> {
    let method = db
        .config()
        .get_as::<String>("notification_method")?
        .unwrap_or_default();

    if method.is_empty() || method == "none" {
        return registry().load("empty", "{}").await;
    }

    let Some(provider) = db.message_senders().get(&method)? else {
        return registry().load("empty", "{}").await;
    };
    registry().load(&method, &provider.addition).await
}
