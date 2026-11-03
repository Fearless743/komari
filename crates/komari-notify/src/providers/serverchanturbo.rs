//! Server酱 Turbo provider (Go `utils/messageSender/serverchanturbo`).

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::{NotifyError, Result};
use crate::http::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport};
use crate::provider::{ConfigField, MessageSender};

/// Configuration keys removed from external API responses.
pub const SENSITIVE_FIELDS: &[&str] = &["api_url", "sendkey", "send_key", "token"];

/// Provider configuration (`serverchanturbo.Addition`).
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerChanTurboConfig {
    pub api_url: String,
    pub channel: String,
    pub noip: String,
    pub openid: String,
}

impl std::fmt::Debug for ServerChanTurboConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted = crate::redact::redact_serialized("serverchanturbo", self);
        f.debug_struct("ServerChanTurboConfig")
            .field("config", &redacted)
            .finish()
    }
}

/// Server酱 Turbo sender.
pub struct ServerChanTurboSender {
    config: ServerChanTurboConfig,
    transport: Arc<dyn HttpTransport>,
}

impl ServerChanTurboSender {
    pub fn new(config: ServerChanTurboConfig) -> Self {
        Self::with_transport(config, Arc::new(ReqwestTransport::default()))
    }

    pub fn with_transport(
        config: ServerChanTurboConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self { config, transport }
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let config: ServerChanTurboConfig = serde_json::from_value(value)?;
        Ok(Self::new(config))
    }

    pub fn with_transport_from_value(
        value: Value,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self> {
        let config: ServerChanTurboConfig = serde_json::from_value(value)?;
        Ok(Self::with_transport(config, transport))
    }

    pub fn payload(&self, message: &str, title: &str) -> Value {
        let mut payload = Map::new();
        payload.insert("title".to_owned(), json!(title.trim()));
        payload.insert("desp".to_owned(), json!(message.trim()));

        if !self.config.channel.trim().is_empty() {
            payload.insert("channel".to_owned(), json!(self.config.channel.trim()));
        }
        if self.config.noip.trim() == "1" {
            payload.insert("noip".to_owned(), json!("1"));
        }
        if !self.config.openid.trim().is_empty() {
            payload.insert("openid".to_owned(), json!(self.config.openid.trim()));
        }
        Value::Object(payload)
    }
}

pub fn fields() -> Vec<ConfigField> {
    vec![
        ConfigField::new("api_url", "string")
            .required(true)
            .help("接口完整地址，例如 https://sctapi.ftqq.com/<sendkey>.send；参考：https://sct.ftqq.com/"),
        ConfigField::new("channel", "string")
            .help("消息通道，可选，多个用 | 隔开，例如 9|66"),
        ConfigField::new("noip", "string")
            .help("是否隐藏调用IP，填 1 隐藏；为空则不隐藏"),
        ConfigField::new("openid", "string")
            .help("抄送 openid，测试号用 , 分隔；企业微信应用用 | 分隔"),
    ]
}

pub fn default_config() -> Value {
    serde_json::to_value(ServerChanTurboConfig::default()).unwrap_or(Value::Null)
}

pub fn constructor(value: Value) -> Result<Box<dyn MessageSender>> {
    Ok(Box::new(ServerChanTurboSender::from_value(value)?))
}

#[async_trait]
impl MessageSender for ServerChanTurboSender {
    fn name(&self) -> &'static str {
        "Server酱Turbo"
    }

    fn config(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    fn sensitive_fields(&self) -> &'static [&'static str] {
        crate::providers::serverchanturbo::SENSITIVE_FIELDS
    }

    async fn send_text(&self, message: &str, title: &str) -> Result<()> {
        let api_url = self.config.api_url.trim();
        if api_url.is_empty() {
            return Err(NotifyError::InvalidConfig(
                "未配置 Server酱 Turbo 接口地址(api_url)".to_owned(),
            ));
        }
        if title.trim().is_empty() && message.trim().is_empty() {
            return Err(NotifyError::InvalidConfig(
                "serverchanturbo: 标题与正文均为空".to_owned(),
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
        let body = response.body_text().trim();
        if body.is_empty() {
            return Err(NotifyError::Send(format!(
                "serverchanturbo: 接口返回非 2xx 状态码: {}",
                response.status
            )));
        }
        return Err(NotifyError::Send(format!(
            "serverchanturbo: 接口返回非 2xx 状态码: {}，响应: {body}",
            response.status
        )));
    }
    Ok(())
}
