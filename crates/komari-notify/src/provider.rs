//! The message-sender contract and provider metadata.
//!
//! This mirrors `utils/messageSender/factory/meta.go`: every provider has a
//! name, a configuration object, and can send a text message. Go's optional
//! `IEventMessageSender` interface is documented here as well, but none of the
//! providers implemented by this crate send structured events directly; they
//! all go through the text template in [`crate::event`].

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

/// Asynchronous text-message provider.
#[async_trait]
pub trait MessageSender: Send + Sync {
    /// Stable provider name, identical to Go's `GetName()`.
    fn name(&self) -> &'static str;

    /// Current configuration as JSON, identical to `GetConfiguration()`.
    ///
    /// This is the full internal configuration and must not be returned from
    /// an external API directly; use [`crate::redact_addition`] instead.
    fn config(&self) -> Value;

    /// Configuration keys that must be removed from API responses.
    fn sensitive_fields(&self) -> &'static [&'static str] {
        &[]
    }

    /// Send one text message. `title` may be empty.
    async fn send_text(&self, message: &str, title: &str) -> Result<()>;

    /// Provider initialisation hook (`Init`).
    async fn init(&self) -> Result<()> {
        Ok(())
    }

    /// Provider teardown hook (`Destroy`).
    async fn destroy(&self) -> Result<()> {
        Ok(())
    }
}

/// Optional capability matching Go's `IEventMessageSender`.
#[async_trait]
pub trait EventMessageSender: MessageSender {
    async fn send_event(&self, event: &komari_db::models::EventMessage) -> Result<()>;
}

/// One configuration field, matching the JSON emitted by `utils/item.Parse`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigField {
    pub name: String,
    pub required: bool,
    #[serde(rename = "type")]
    pub field_type: String,
    pub options: String,
    pub default: String,
    pub help: String,
}

impl ConfigField {
    pub fn new(name: impl Into<String>, field_type: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            required: false,
            field_type: field_type.into(),
            options: String::new(),
            default: String::new(),
            help: String::new(),
        }
    }

    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    pub fn options(mut self, options: impl Into<String>) -> Self {
        self.options = options.into();
        self
    }

    pub fn default_value(mut self, default: impl Into<String>) -> Self {
        self.default = default.into();
        self
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = help.into();
        self
    }
}
