//! Bark provider (Go `utils/messageSender/bark`).

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::{NotifyError, Result};
use crate::http::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport};
use crate::provider::{ConfigField, MessageSender};

/// Provider configuration (`bark.Addition`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BarkConfig {
    pub server_url: String,
    pub device_key: String,
    pub icon: String,
    pub level: String,
}

impl Default for BarkConfig {
    fn default() -> Self {
        Self {
            server_url: "https://api.day.app".to_owned(),
            device_key: String::new(),
            icon: String::new(),
            level: "timeSensitive".to_owned(),
        }
    }
}

/// Bark text sender.
pub struct BarkSender {
    config: BarkConfig,
    transport: Arc<dyn HttpTransport>,
}

impl BarkSender {
    pub fn new(config: BarkConfig) -> Self {
        Self::with_transport(config, Arc::new(ReqwestTransport::default()))
    }

    pub fn with_transport(config: BarkConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self { config, transport }
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let config: BarkConfig = serde_json::from_value(value)?;
        let sender = Self::new(config);
        sender.init_sync()?;
        Ok(sender)
    }

    pub fn with_transport_from_value(
        value: Value,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self> {
        let config: BarkConfig = serde_json::from_value(value)?;
        let sender = Self::with_transport(config, transport);
        sender.init_sync()?;
        Ok(sender)
    }

    /// Same check as Go `BarkSender.Init`: an explicitly configured server URL
    /// must parse.
    fn init_sync(&self) -> Result<()> {
        if !self.config.server_url.is_empty() {
            reqwest::Url::parse(&self.config.server_url).map_err(|error| {
                NotifyError::InvalidConfig(format!("invalid server URL: {error}"))
            })?;
        }
        Ok(())
    }

    pub fn push_url(&self) -> String {
        let server_url = if self.config.server_url.is_empty() {
            "https://api.day.app"
        } else {
            self.config.server_url.as_str()
        };
        format!("{}/push", server_url.trim_end_matches('/'))
    }

    pub fn payload(&self, message: &str, title: &str) -> Value {
        let mut payload = Map::new();
        payload.insert("body".to_owned(), json!(message));
        payload.insert("device_key".to_owned(), json!(self.config.device_key));
        if !title.is_empty() {
            payload.insert("title".to_owned(), json!(title));
        }
        if !self.config.icon.is_empty() {
            payload.insert("icon".to_owned(), json!(self.config.icon));
        }
        if !self.config.level.is_empty() {
            payload.insert("level".to_owned(), json!(self.config.level));
        }
        Value::Object(payload)
    }
}

pub fn fields() -> Vec<ConfigField> {
    vec![
        ConfigField::new("server_url", "string")
            .required(true)
            .default_value("https://api.day.app")
            .help("Bark server URL, e.g., https://api.day.app or your self-hosted server address"),
        ConfigField::new("device_key", "string")
            .required(true)
            .help("Your Bark device key, which can be found in the Bark App"),
        ConfigField::new("icon", "string")
            .help("Push notification icon, supports URL or system icon name"),
        ConfigField::new("level", "option")
            .default_value("timeSensitive")
            .options("active,timeSensitive,passive,critical")
            .help("Push notification level: active, timeSensitive (default), passive, critical"),
    ]
}

pub fn default_config() -> Value {
    serde_json::to_value(BarkConfig::default()).unwrap_or(Value::Null)
}

pub fn constructor(value: Value) -> Result<Box<dyn MessageSender>> {
    Ok(Box::new(BarkSender::from_value(value)?))
}

#[async_trait]
impl MessageSender for BarkSender {
    fn name(&self) -> &'static str {
        "bark"
    }

    fn config(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    async fn init(&self) -> Result<()> {
        self.init_sync()
    }

    async fn send_text(&self, message: &str, title: &str) -> Result<()> {
        if self.config.device_key.is_empty() {
            return Err(NotifyError::InvalidConfig(
                "device key is required".to_owned(),
            ));
        }
        if message.is_empty() {
            return Err(NotifyError::EmptyMessage);
        }

        let body = serde_json::to_vec(&self.payload(message, title))?;
        let request = HttpRequest::post(self.push_url())
            .header("Content-Type", "application/json")
            .body(body);
        let response = self.transport.execute(request).await?;
        check_response(response)
    }
}

fn check_response(response: HttpResponse) -> Result<()> {
    if response.status != 200 {
        return Err(NotifyError::Send(format!(
            "bark API returned non-OK status: {}",
            response.status
        )));
    }

    #[derive(Deserialize)]
    struct BarkResponse {
        code: i64,
        #[serde(default)]
        message: String,
    }

    // Some self-hosted Bark servers return plain text; Go treats a decode
    // failure as success.
    let Ok(parsed) = serde_json::from_slice::<BarkResponse>(&response.body) else {
        return Ok(());
    };
    if parsed.code != 200 {
        return Err(NotifyError::Send(format!(
            "bark API error (code: {}): {}",
            parsed.code, parsed.message
        )));
    }
    Ok(())
}
