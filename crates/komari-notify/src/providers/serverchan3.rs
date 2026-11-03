//! Server酱³ provider (Go `utils/messageSender/serverchan3`).

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::{NotifyError, Result};
use crate::http::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport};
use crate::provider::{ConfigField, MessageSender};

/// Configuration keys removed from external API responses.
pub const SENSITIVE_FIELDS: &[&str] = &["api_url", "sendkey", "send_key", "token"];

/// Provider configuration (`serverchan3.Addition`).
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerChan3Config {
    pub api_url: String,
    pub tags: String,
}

impl std::fmt::Debug for ServerChan3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted = crate::redact::redact_serialized("Server酱³", self);
        f.debug_struct("ServerChan3Config")
            .field("config", &redacted)
            .finish()
    }
}

/// Server酱³ sender.
pub struct ServerChan3Sender {
    config: ServerChan3Config,
    transport: Arc<dyn HttpTransport>,
}

impl ServerChan3Sender {
    pub fn new(config: ServerChan3Config) -> Self {
        Self::with_transport(config, Arc::new(ReqwestTransport::default()))
    }

    pub fn with_transport(config: ServerChan3Config, transport: Arc<dyn HttpTransport>) -> Self {
        Self { config, transport }
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let config: ServerChan3Config = crate::provider::deserialize_config("Server酱³", value)?;
        Ok(Self::new(config))
    }

    pub fn with_transport_from_value(
        value: Value,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self> {
        let config: ServerChan3Config = crate::provider::deserialize_config("Server酱³", value)?;
        Ok(Self::with_transport(config, transport))
    }

    pub fn payload(&self, message: &str, title: &str) -> Value {
        let mut payload = Map::new();
        payload.insert("title".to_owned(), json!(title.trim()));
        payload.insert("desp".to_owned(), json!(message.trim()));

        if !self.config.tags.trim().is_empty() {
            let tags = self
                .config
                .tags
                .split('|')
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .collect::<Vec<_>>();
            if !tags.is_empty() {
                payload.insert("tags".to_owned(), json!(tags.join("|")));
            }
        }
        Value::Object(payload)
    }
}

pub fn fields() -> Vec<ConfigField> {
    vec![
        ConfigField::new("api_url", "string")
            .required(true)
            .help("接口完整地址，例如 https://<uid>.push.ft07.com/send/<sendkey>.send；参考：https://sc3.ft07.com/"),
        ConfigField::new("tags", "string")
            .help("可选标签，使用 | 分割，例如 tag1|tag2|tag3"),
    ]
}

pub fn default_config() -> Value {
    serde_json::to_value(ServerChan3Config::default()).unwrap_or(Value::Null)
}

pub fn constructor(value: Value) -> Result<Box<dyn MessageSender>> {
    Ok(Box::new(ServerChan3Sender::from_value(value)?))
}

#[async_trait]
impl MessageSender for ServerChan3Sender {
    fn name(&self) -> &'static str {
        "Server酱³"
    }

    fn config(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    fn sensitive_fields(&self) -> &'static [&'static str] {
        crate::providers::serverchan3::SENSITIVE_FIELDS
    }

    async fn send_text(&self, message: &str, title: &str) -> Result<()> {
        let api_url = self.config.api_url.trim();
        if api_url.is_empty() {
            return Err(NotifyError::InvalidConfig(
                "未配置 Server酱³ 接口地址(api_url)".to_owned(),
            ));
        }
        if title.trim().is_empty() && message.trim().is_empty() {
            return Err(NotifyError::InvalidConfig(
                "serverchan3: 标题与正文均为空".to_owned(),
            ));
        }

        let body = serde_json::to_vec(&self.payload(message, title))?;
        let request = HttpRequest::post(api_url)
            .header("Content-Type", "application/json")
            .body(body);
        let response = self.transport.execute(request).await?;
        check_response(response)
    }
}

fn check_response(response: HttpResponse) -> Result<()> {
    if !(200..300).contains(&response.status) {
        // The remote body may contain credentials; only report the status.
        return Err(NotifyError::Send(format!(
            "接口返回非 2xx 状态码: {}",
            response.status
        )));
    }
    Ok(())
}
