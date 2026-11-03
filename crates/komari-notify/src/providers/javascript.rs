//! JavaScript provider registration.
//!
//! Go executes user scripts through `goja` (`utils/messageSender/javascript`).
//! There is no Rust runtime with identical semantics, so the provider remains
//! visible to the factory (so existing configuration rows can be listed) but
//! every attempt to use it returns [`NotifyError::Unsupported`]. This is a
//! documented parity gap, not a silent fallback.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{NotifyError, Result};
use crate::provider::{ConfigField, MessageSender};

/// Configuration keys removed from external API responses.
pub const SENSITIVE_FIELDS: &[&str] = &["script", "code"];

/// Provider configuration (`javascript.Addition`).
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JavaScriptConfig {
    pub script: String,
}

impl std::fmt::Debug for JavaScriptConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted = crate::redact::redact_serialized("Javascript", self);
        f.debug_struct("JavaScriptConfig")
            .field("config", &redacted)
            .finish()
    }
}

/// Placeholder for Go's `JavaScriptSender`.
pub struct JavaScriptSender {
    config: JavaScriptConfig,
}

impl JavaScriptSender {
    pub fn new(config: JavaScriptConfig) -> Self {
        Self { config }
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let config: JavaScriptConfig = serde_json::from_value(value)?;
        Ok(Self::new(config))
    }

    fn unsupported() -> NotifyError {
        NotifyError::Unsupported(
            "the Javascript message sender requires goja; use a native provider instead".to_owned(),
        )
    }
}

pub fn fields() -> Vec<ConfigField> {
    vec![ConfigField::new("script", "richtext").required(true).help(
        "JavaScript code (ES6 supported) that implements sendMessage(message, title) and optionally sendEvent(event) functions. Both should return a Promise or boolean. Available APIs: fetch(), xhr(), console.log().",
    )]
}

pub fn default_config() -> Value {
    serde_json::to_value(JavaScriptConfig::default()).unwrap_or(Value::Null)
}

pub fn constructor(value: Value) -> Result<Box<dyn MessageSender>> {
    Ok(Box::new(JavaScriptSender::from_value(value)?))
}

#[async_trait]
impl MessageSender for JavaScriptSender {
    fn name(&self) -> &'static str {
        "Javascript"
    }

    fn config(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    fn sensitive_fields(&self) -> &'static [&'static str] {
        crate::providers::javascript::SENSITIVE_FIELDS
    }

    async fn init(&self) -> Result<()> {
        Err(Self::unsupported())
    }

    async fn send_text(&self, _message: &str, _title: &str) -> Result<()> {
        Err(Self::unsupported())
    }
}
