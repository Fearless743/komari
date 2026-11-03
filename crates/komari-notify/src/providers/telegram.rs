//! Telegram Bot API provider (Go `utils/messageSender/telegram`).

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{NotifyError, Result};
use crate::http::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, form_encode};
use crate::provider::{ConfigField, MessageSender};

/// Configuration keys removed from external API responses.
pub const SENSITIVE_FIELDS: &[&str] = &["bot_token"];

/// Provider configuration (`telegram.Addition`).
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: String,
    pub message_thread_id: String,
    pub endpoint: String,
}

impl std::fmt::Debug for TelegramConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted = crate::redact::redact_serialized("telegram", self);
        f.debug_struct("TelegramConfig")
            .field("config", &redacted)
            .finish()
    }
}

/// Telegram text sender.
pub struct TelegramSender {
    config: TelegramConfig,
    transport: Arc<dyn HttpTransport>,
}

impl TelegramSender {
    pub fn new(config: TelegramConfig) -> Self {
        Self::with_transport(config, Arc::new(ReqwestTransport::without_timeout()))
    }

    pub fn with_transport(config: TelegramConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self { config, transport }
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let config: TelegramConfig = crate::provider::deserialize_config("telegram", value)?;
        Ok(Self::new(config))
    }

    pub fn with_transport_from_value(
        value: Value,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self> {
        let config: TelegramConfig = crate::provider::deserialize_config("telegram", value)?;
        Ok(Self::with_transport(config, transport))
    }

    /// The URL constructed exactly like Go: endpoint + token + `/sendMessage`.
    pub fn endpoint_url(&self) -> String {
        format!(
            "{}{}/sendMessage",
            self.config.endpoint, self.config.bot_token
        )
    }

    /// Form body sent to the Bot API.
    pub fn request_body(&self, message: &str, title: &str) -> String {
        let full_message = if title.is_empty() {
            message.to_owned()
        } else {
            format!("<b>{title}</b>\n{message}")
        };

        let mut pairs = vec![
            ("chat_id", self.config.chat_id.as_str()),
            ("text", full_message.as_str()),
            ("parse_mode", "HTML"),
        ];
        if !self.config.message_thread_id.is_empty() {
            pairs.push(("message_thread_id", self.config.message_thread_id.as_str()));
        }
        // Go stores form values in a map and `url.Values.Encode` sorts keys.
        pairs.sort_by(|left, right| left.0.cmp(right.0));
        form_encode(&pairs)
    }
}

pub fn fields() -> Vec<ConfigField> {
    vec![
        ConfigField::new("bot_token", "string").required(true),
        ConfigField::new("chat_id", "string").required(true),
        ConfigField::new("message_thread_id", "string").help(
            "Optional. Unique identifier of a message thread to which the message belongs; for supergroups only",
        ),
        ConfigField::new("endpoint", "string")
            .required(true)
            .default_value("https://api.telegram.org/bot")
            .help("Telegram API endpoint, default is https://api.telegram.org/bot"),
    ]
}

pub fn default_config() -> Value {
    serde_json::to_value(TelegramConfig::default()).unwrap_or(Value::Null)
}

pub fn constructor(value: Value) -> Result<Box<dyn MessageSender>> {
    Ok(Box::new(TelegramSender::from_value(value)?))
}

#[async_trait]
impl MessageSender for TelegramSender {
    fn name(&self) -> &'static str {
        "telegram"
    }

    fn config(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    fn sensitive_fields(&self) -> &'static [&'static str] {
        crate::providers::telegram::SENSITIVE_FIELDS
    }

    async fn send_text(&self, message: &str, title: &str) -> Result<()> {
        let full_message = if title.is_empty() {
            message.to_owned()
        } else {
            format!("<b>{title}</b>\n{message}")
        };
        if full_message.is_empty() {
            return Err(NotifyError::EmptyMessage);
        }

        let request = HttpRequest::post(self.endpoint_url())
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(self.request_body(message, title).into_bytes());

        let response = self.transport.execute(request).await?;
        check_response(response)
    }
}

fn check_response(response: HttpResponse) -> Result<()> {
    if response.status != 200 {
        return Err(NotifyError::Send(format!(
            "telegram API returned non-OK status: {}",
            response.status
        )));
    }

    #[derive(Deserialize)]
    struct TelegramResponse {
        ok: bool,
        #[serde(default, rename = "description")]
        _description: String,
    }

    let parsed: TelegramResponse = serde_json::from_slice(&response.body)?;
    if !parsed.ok {
        // Telegram descriptions can echo chat/user data; keep the error generic.
        return Err(NotifyError::Send("telegram API error".to_owned()));
    }
    Ok(())
}
