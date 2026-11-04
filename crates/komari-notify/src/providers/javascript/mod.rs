//! JavaScript message sender (Go `utils/messageSender/javascript`).
//!
//! Go executes user scripts with `goja`; this port embeds `boa_engine`, a
//! pure-Rust ECMAScript engine. The script contract is the same:
//!
//! * `sendMessage(message, title)` is required and may return a boolean or a
//!   Promise. A falsy value or a rejected Promise is a delivery failure.
//! * `sendEvent(event)` is optional. Without it, the provider falls back to
//!   [`JavaScriptSender::fallback_message`] and `sendMessage`, like Go.
//! * `fetch`, `XMLHttpRequest`, `console`, `setTimeout`, `clearTimeout` and
//!   `require` are injected before the script runs.
//!
//! Execution is bounded by [`DEFAULT_TIMEOUT`] and by a loop-iteration limit so
//! a runaway script cannot hang a notification worker forever.
//!
//! Errors never echo the script text or a JavaScript error message: only the
//! error class survives, because both the script and the values it
//! interpolates into errors may contain credentials.

mod engine;
mod net;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use komari_db::models::EventMessage;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::http::{HttpTransport, ReqwestTransport};
use crate::provider::{ConfigField, EventMessageSender, MessageSender};

/// Configuration keys removed from external API responses.
pub const SENSITIVE_FIELDS: &[&str] = &["script", "code"];

/// Wall-clock limit for one script invocation (Go uses 30 seconds).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

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

/// Go's `JavaScriptSender`.
pub struct JavaScriptSender {
    config: JavaScriptConfig,
    transport: Arc<dyn HttpTransport>,
    timeout: Duration,
}

impl JavaScriptSender {
    /// Build a sender using the production HTTP transport.
    pub fn new(config: JavaScriptConfig) -> Self {
        Self::with_transport(config, Arc::new(ReqwestTransport::default()))
    }

    /// Build a sender with an explicit transport (tests, custom clients).
    pub fn with_transport(config: JavaScriptConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            config,
            transport,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Override the per-invocation wall-clock limit.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let config: JavaScriptConfig = crate::provider::deserialize_config("Javascript", value)?;
        Ok(Self::new(config))
    }

    /// Text used when a script does not define `sendEvent`.
    ///
    /// Port of `fallbackToTextMessage`: the emoji is repeated three times, the
    /// client list is rendered like Go's `%s` on a `[]string` (`[a b]`), names
    /// fall back to UUIDs and time is RFC 3339.
    pub fn fallback_message(event: &EventMessage) -> String {
        let time = event
            .time
            .to_rfc3339()
            .unwrap_or_else(|| "0001-01-01T00:00:00Z".to_owned());

        if event.clients.is_empty() {
            format!(
                "{emoji}{emoji}{emoji}\nEvent: {event}\nMessage: {message}\nTime: {time}",
                emoji = event.emoji,
                event = event.event,
                message = event.message,
            )
        } else {
            let names = event
                .clients
                .iter()
                .map(|client| {
                    if client.name.is_empty() {
                        client.uuid.as_str()
                    } else {
                        client.name.as_str()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!(
                "{emoji}{emoji}{emoji}\nEvent: {event}\nClients: [{names}]\nMessage: {message}\nTime: {time}",
                emoji = event.emoji,
                event = event.event,
                message = event.message,
            )
        }
    }

    async fn execute(&self, invocation: engine::Invocation) -> Result<()> {
        engine::execute(
            self.config.script.clone(),
            self.transport.clone(),
            self.timeout,
            invocation,
        )
        .await
    }
}

pub fn fields() -> Vec<ConfigField> {
    vec![ConfigField::new("script", "richtext").required(true).help(
        "JavaScript code (ES6 supported) that implements sendMessage(message, title) and optionally sendEvent(event) functions. Both should return a Promise or boolean. Available APIs: fetch(), XMLHttpRequest, console.log(), setTimeout(), require('console'|'process').",
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
        SENSITIVE_FIELDS
    }

    /// Go's `SendEvent` type-asserts the provider to `IEventMessageSender`.
    fn as_event_sender(&self) -> Option<&dyn EventMessageSender> {
        Some(self)
    }

    async fn init(&self) -> Result<()> {
        self.execute(engine::Invocation::Validate).await
    }

    async fn send_text(&self, message: &str, title: &str) -> Result<()> {
        self.execute(engine::Invocation::Text {
            message: message.to_owned(),
            title: title.to_owned(),
        })
        .await
    }
}

#[async_trait]
impl EventMessageSender for JavaScriptSender {
    async fn send_event(&self, event: &EventMessage) -> Result<()> {
        self.execute(engine::Invocation::Event(Box::new(event.clone())))
            .await
    }
}

#[cfg(test)]
mod tests {
    use komari_db::LocalTime;
    use komari_db::models::{Client, EventMessage};

    use super::*;

    #[test]
    fn fallback_message_matches_go_without_clients() {
        let event = EventMessage {
            event: "Offline".to_owned(),
            time: LocalTime::parse("2026-01-02T03:04:05Z").unwrap(),
            message: "gone".to_owned(),
            emoji: "🔴".to_owned(),
            ..EventMessage::default()
        };
        let message = JavaScriptSender::fallback_message(&event);
        assert_eq!(
            message,
            "🔴🔴🔴\nEvent: Offline\nMessage: gone\nTime: 2026-01-02T03:04:05Z"
        );
    }

    #[test]
    fn fallback_message_renders_clients_like_go_percent_s() {
        let event = EventMessage {
            event: "Offline".to_owned(),
            clients: vec![
                Client {
                    name: "alpha".to_owned(),
                    uuid: "u-1".to_owned(),
                    ..Client::default()
                },
                Client {
                    name: String::new(),
                    uuid: "u-2".to_owned(),
                    ..Client::default()
                },
            ],
            time: LocalTime::parse("2026-01-02T03:04:05Z").unwrap(),
            message: "gone".to_owned(),
            emoji: "🔴".to_owned(),
        };
        let message = JavaScriptSender::fallback_message(&event);
        assert_eq!(
            message,
            "🔴🔴🔴\nEvent: Offline\nClients: [alpha u-2]\nMessage: gone\nTime: 2026-01-02T03:04:05Z"
        );
    }

    #[test]
    fn config_debug_hides_the_script() {
        let config = JavaScriptConfig {
            script: "JS_DEBUG_SECRET".to_owned(),
        };
        assert!(!format!("{config:?}").contains("JS_DEBUG_SECRET"));
    }
}
