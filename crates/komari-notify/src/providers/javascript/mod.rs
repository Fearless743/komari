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
mod worker;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use komari_db::models::EventMessage;
use serde::Serialize;
use serde_json::Value;
use tokio::runtime::Handle;

use crate::error::{NotifyError, Result};
use crate::http::{HttpTransport, ReqwestTransport};
use crate::provider::{ConfigField, EventMessageSender, MessageSender};
use worker::Worker;

/// Configuration keys removed from external API responses.
pub const SENSITIVE_FIELDS: &[&str] = &["script", "code"];

/// Wall-clock limit for one script invocation (Go uses 30 seconds).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Provider configuration (`javascript.Addition`).
#[derive(Clone, Default, Serialize)]
pub struct JavaScriptConfig {
    pub script: String,
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
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
///
/// The `boa_engine` context is `!Send`, so it lives on a dedicated thread (see
/// [`worker::Worker`]) that is created on `Init` and reused for every message,
/// matching Go's single `goja.Runtime` per provider instance.
pub struct JavaScriptSender {
    config: JavaScriptConfig,
    transport: Arc<dyn HttpTransport>,
    timeout: Duration,
    worker: Mutex<Option<Arc<Worker>>>,
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
            worker: Mutex::new(None),
        }
    }

    /// Override the per-invocation wall-clock limit.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Parse `addition` like Go's `encoding/json` into `Addition`.
    ///
    /// Field names are matched case-insensitively, `null` behaves like the
    /// zero value and unknown fields are ignored. Type errors name the field
    /// and the JSON type only, never the value.
    pub fn from_value(value: Value) -> Result<Self> {
        let map = match value {
            Value::Null => return Ok(Self::new(JavaScriptConfig::default())),
            Value::Object(map) => map,
            other => {
                return Err(NotifyError::InvalidConfig(format!(
                    "failed to load config for provider Javascript: expected an object, found {}",
                    json_type(&other)
                )));
            }
        };

        let mut config = JavaScriptConfig::default();
        for (key, value) in map {
            if !key.eq_ignore_ascii_case("script") {
                continue;
            }
            config.script = match value {
                Value::Null => String::new(),
                Value::String(script) => script,
                other => {
                    return Err(NotifyError::InvalidConfig(format!(
                        "failed to load config for provider Javascript: field script: expected a string, found {}",
                        json_type(&other)
                    )));
                }
            };
        }
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

    /// Return the live runtime thread, starting it (Go `Init`) if needed.
    async fn worker(&self) -> Result<Arc<Worker>> {
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .cloned()
        {
            return Ok(worker);
        }

        let handle = Handle::try_current().map_err(|_| {
            NotifyError::Send("JavaScript provider requires a Tokio runtime".to_owned())
        })?;
        let (worker, ready) =
            Worker::spawn(self.config.script.clone(), self.transport.clone(), handle);
        ready.await.map_err(|_| {
            NotifyError::Send("JavaScript runtime terminated during init".to_owned())
        })??;
        let worker = Arc::new(worker);

        // A concurrent caller may have won the race; keep one runtime so the
        // script is not loaded twice.
        let mut slot = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = slot.as_ref() {
            return Ok(existing.clone());
        }
        *slot = Some(worker.clone());
        Ok(worker)
    }

    /// Drop the runtime thread. A stuck script keeps running until it hits the
    /// loop-iteration limit, but the next call starts a fresh runtime.
    fn poison(&self) {
        let _ = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    /// Run one invocation with the Go 30-second wall-clock limit.
    ///
    /// The limit wraps the whole call, including synchronous script code that
    /// never yields to the promise loop. Go only enforces it while polling a
    /// Promise; a synchronous infinite loop there hangs forever.
    /// JSON object passed to `sendEvent`, matching `json.Marshal(EventMessage)`.
    ///
    /// `EventMessage.Time` is a plain `time.Time` in Go, so it marshals as
    /// RFC 3339 with nanoseconds and the application offset; a nil `Clients`
    /// slice marshals as `null`. `komari-db` cannot distinguish an empty slice
    /// from a nil one, so an empty list is sent as `null` like Go's nil slice.
    pub fn event_json(event: &EventMessage) -> Value {
        let clients = if event.clients.is_empty() {
            Value::Null
        } else {
            serde_json::to_value(&event.clients).unwrap_or(Value::Null)
        };
        serde_json::json!({
            "event": &event.event,
            "clients": clients,
            "time": event.time.to_rfc3339_nano(),
            "message": &event.message,
            "emoji": &event.emoji,
        })
    }

    async fn execute(&self, invocation: engine::Invocation) -> Result<()> {
        let worker = self.worker().await?;
        let reply = worker.invoke(invocation, self.timeout)?;

        match tokio::time::timeout(self.timeout, reply).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                self.poison();
                Err(NotifyError::Send(
                    "JavaScript runtime terminated unexpectedly".to_owned(),
                ))
            }
            Err(_) => {
                self.poison();
                Err(NotifyError::Send("JavaScript execution timeout".to_owned()))
            }
        }
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
        self.worker().await.map(|_| ())
    }

    async fn destroy(&self) -> Result<()> {
        self.poison();
        Ok(())
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
        let time = event.time.to_rfc3339().unwrap();
        assert_eq!(
            message,
            format!("🔴🔴🔴\nEvent: Offline\nMessage: gone\nTime: {time}")
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
        let time = event.time.to_rfc3339().unwrap();
        assert_eq!(
            message,
            format!("🔴🔴🔴\nEvent: Offline\nClients: [alpha u-2]\nMessage: gone\nTime: {time}")
        );
    }

    #[test]
    fn event_json_matches_go_time_and_nil_clients() {
        let mut event = EventMessage {
            event: "Login".to_owned(),
            time: LocalTime::parse("2026-01-02T03:04:05.123456789Z").unwrap(),
            message: "new login".to_owned(),
            emoji: "🔑".to_owned(),
            ..EventMessage::default()
        };
        let json = JavaScriptSender::event_json(&event);
        assert!(json["clients"].is_null());
        assert_eq!(json["time"], event.time.to_rfc3339_nano());
        assert!(json["time"].as_str().unwrap().contains(".123456789"));

        // Go's zero time marshals as the zero RFC 3339 instant.
        event.time = LocalTime::zero();
        let json = JavaScriptSender::event_json(&event);
        assert_eq!(json["time"], "0001-01-01T00:00:00Z");
    }

    #[test]
    fn config_debug_hides_the_script() {
        let config = JavaScriptConfig {
            script: "JS_DEBUG_SECRET".to_owned(),
        };
        assert!(!format!("{config:?}").contains("JS_DEBUG_SECRET"));
    }
}
