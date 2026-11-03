//! Provider abstraction shared by all DDNS backends.
//!
//! The trait mirrors Go's `factory.IDdnsProvider` and the request/response
//! types mirror `factory.SyncContext` / `factory.SyncResult`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::DdnsError;

/// A provider configuration as stored in `ddns_providers.addition`.
pub type ProviderConfig = serde_json::Map<String, Value>;

/// Metadata for one configuration field, mirroring Go's `utils/item.Item`.
///
/// The Go backend derives these entries from struct tags and exposes them to
/// the admin UI through `factory.GetProviderConfigs`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigurationItem {
    pub name: String,
    pub required: bool,
    #[serde(rename = "type")]
    pub item_type: String,
    pub options: String,
    pub default: String,
    pub help: String,
}

impl ConfigurationItem {
    /// Build an item with the same defaults as `item.Parse`: the type falls
    /// back to `string` when it is empty.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            item_type: "string".to_owned(),
            ..Self::default()
        }
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn item_type(mut self, item_type: &str) -> Self {
        self.item_type = item_type.to_owned();
        self
    }

    pub fn options(mut self, options: &str) -> Self {
        self.options = options.to_owned();
        self
    }

    pub fn default_value(mut self, default: &str) -> Self {
        self.default = default.to_owned();
        self
    }

    pub fn help(mut self, help: &str) -> Self {
        self.help = help.to_owned();
        self
    }
}

/// Everything a provider needs to synchronize one client.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncContext {
    pub ipv4: String,
    pub ipv6: String,
    pub client_uuid: String,
    pub client_name: String,
    pub triggered_by: String,
    pub force: bool,
    #[serde(skip_serializing_if = "ProviderConfig::is_empty")]
    pub provider_config: ProviderConfig,
}

/// The outcome of one provider synchronization.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncResult {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub resolved_record_id: String,
}

impl SyncResult {
    pub fn resolved(record_id: impl Into<String>) -> Self {
        Self {
            resolved_record_id: record_id.into(),
        }
    }
}

/// Read a string value from a provider config, mirroring Go's
/// `getString` helper (non-string values are ignored).
pub fn config_string(config: &ProviderConfig, key: &str) -> String {
    config
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A DDNS backend.
///
/// Implementations are registered with [`crate::factory::DdnsFactory`] and
/// receive a fresh [`SyncContext`] for every client. The trait is object safe
/// and can be shared across tasks.
#[async_trait]
pub trait DdnsProvider: Send + Sync {
    /// Stable registry name, for example `cloudflare`.
    fn name(&self) -> &str;

    /// Current configuration serialized with the same field names as Go.
    fn configuration(&self) -> Value;

    /// Replace the configuration, mirroring `json.Unmarshal` into the Go
    /// provider's `GetConfiguration()` pointer.
    fn set_configuration(&mut self, configuration: Value) -> Result<(), DdnsError>;

    /// Configuration fields exposed to the admin UI.
    fn configuration_items(&self) -> Vec<ConfigurationItem> {
        Vec::new()
    }

    /// Validate and prepare the provider. Called after `set_configuration`.
    fn init(&mut self) -> Result<(), DdnsError> {
        Ok(())
    }

    /// Release resources. Called when a provider is replaced.
    fn destroy(&self) -> Result<(), DdnsError> {
        Ok(())
    }

    /// Create or update the DNS record for one client.
    async fn sync(&self, context: &SyncContext) -> Result<SyncResult, DdnsError>;
}
