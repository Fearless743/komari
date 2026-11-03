//! Generic webhook provider (Go `utils/messageSender/webhook`).

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{NotifyError, Result};
use crate::http::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport};
use crate::provider::{ConfigField, MessageSender};

/// Default request body from `webhook.Addition`.
pub const DEFAULT_BODY: &str = r#"{"message":"{{message}}","title":"{{title}}"}"#;

/// Configuration keys removed from external API responses.
pub const SENSITIVE_FIELDS: &[&str] = &["headers", "body", "password"];

/// Provider configuration (`webhook.Addition`).
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebhookConfig {
    pub url: String,
    pub method: String,
    pub content_type: String,
    pub headers: String,
    pub body: String,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for WebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted = crate::redact::redact_serialized("webhook", self);
        f.debug_struct("WebhookConfig")
            .field("config", &redacted)
            .finish()
    }
}

/// Webhook sender.
pub struct WebhookSender {
    config: WebhookConfig,
    transport: Arc<dyn HttpTransport>,
}

impl WebhookSender {
    pub fn new(config: WebhookConfig) -> Self {
        Self::with_transport(config, Arc::new(ReqwestTransport::default()))
    }

    pub fn with_transport(config: WebhookConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self { config, transport }
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let config: WebhookConfig = serde_json::from_value(value)?;
        Ok(Self::new(config))
    }

    pub fn with_transport_from_value(
        value: Value,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self> {
        let config: WebhookConfig = serde_json::from_value(value)?;
        Ok(Self::with_transport(config, transport))
    }

    /// Build the request exactly like Go's `createPOSTRequest`/`createGETRequest`.
    pub fn build_request(&self, message: &str, title: &str) -> Result<HttpRequest> {
        if self.config.url.is_empty() {
            return Err(NotifyError::InvalidConfig(
                "webhook URL is not configured".to_owned(),
            ));
        }

        let method = if self.config.method.is_empty() {
            "GET".to_owned()
        } else {
            self.config.method.to_uppercase()
        };

        let request = match method.as_str() {
            "POST" => self.create_post_request(message, title)?,
            "GET" => self.create_get_request(message, title)?,
            other => {
                return Err(NotifyError::Send(format!(
                    "unsupported HTTP method: {other}"
                )));
            }
        };

        let mut request = request;
        if !self.config.headers.is_empty() {
            if let Ok(headers) = serde_json::from_str::<std::collections::HashMap<String, String>>(
                &self.config.headers,
            ) {
                for (key, value) in headers {
                    request = request.set_header(key, value);
                }
            }
        }

        if !self.config.username.is_empty() && !self.config.password.is_empty() {
            let credentials = format!("{}:{}", self.config.username, self.config.password);
            let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
            request = request.set_header("Authorization", format!("Basic {encoded}"));
        }

        Ok(request)
    }

    fn create_post_request(&self, message: &str, title: &str) -> Result<HttpRequest> {
        let content_type = if self.config.content_type.is_empty() {
            "application/json"
        } else {
            self.config.content_type.as_str()
        };

        let body = if is_json_content_type(content_type) {
            self.replace_template_json(&self.config.body, message, title)
        } else {
            self.replace_template(&self.config.body, message, title)
        };

        Ok(HttpRequest::post(&self.config.url)
            .set_header("Content-Type", content_type)
            .body(body.into_bytes()))
    }

    fn create_get_request(&self, message: &str, title: &str) -> Result<HttpRequest> {
        let url = self.replace_template(&self.config.url, message, title);
        reqwest::Url::parse(&url)
            .map_err(|error| NotifyError::InvalidConfig(format!("invalid URL: {error}")))?;
        Ok(HttpRequest::get(url))
    }

    fn replace_template(&self, template: &str, message: &str, title: &str) -> String {
        template
            .replace("{{message}}", message)
            .replace("{{title}}", title)
    }

    fn replace_template_json(&self, template: &str, message: &str, title: &str) -> String {
        template
            .replace("{{message}}", &json_unicode_escape_string(message))
            .replace("{{title}}", &json_unicode_escape_string(title))
    }
}

fn is_json_content_type(content_type: &str) -> bool {
    content_type.to_lowercase().contains("application/json")
}

/// Escape a value for insertion inside a JSON string literal, using the
/// `\uXXXX` forms Go's webhook provider emits.
pub fn json_unicode_escape_string(value: &str) -> String {
    use std::fmt::Write;

    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\u0022"),
            '\\' => escaped.push_str("\\u005C"),
            '\u{8}' => escaped.push_str("\\u0008"),
            '\u{c}' => escaped.push_str("\\u000C"),
            '\n' => escaped.push_str("\\u000A"),
            '\r' => escaped.push_str("\\u000D"),
            '\t' => escaped.push_str("\\u0009"),
            control if (control as u32) < 0x20 => {
                let _ = write!(escaped, "\\u{:04X}", control as u32);
            }
            other => escaped.push(other),
        }
    }
    escaped
}

pub fn fields() -> Vec<ConfigField> {
    vec![
        ConfigField::new("url", "string").required(true),
        ConfigField::new("method", "option")
            .default_value("GET")
            .options("POST,GET"),
        ConfigField::new("content_type", "string").default_value("application/json"),
        ConfigField::new("headers", "string").help("HTTP headers in JSON format"),
        ConfigField::new("body", "string").default_value(DEFAULT_BODY),
        ConfigField::new("username", "string"),
        ConfigField::new("password", "string"),
    ]
}

pub fn default_config() -> Value {
    serde_json::to_value(WebhookConfig::default()).unwrap_or(Value::Null)
}

pub fn constructor(value: Value) -> Result<Box<dyn MessageSender>> {
    Ok(Box::new(WebhookSender::from_value(value)?))
}

#[async_trait]
impl MessageSender for WebhookSender {
    fn name(&self) -> &'static str {
        "webhook"
    }

    fn config(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    fn sensitive_fields(&self) -> &'static [&'static str] {
        crate::providers::webhook::SENSITIVE_FIELDS
    }

    async fn send_text(&self, message: &str, title: &str) -> Result<()> {
        let request = self.build_request(message, title)?;
        let response = self.transport.execute(request).await?;
        check_response(response)
    }
}

fn check_response(response: HttpResponse) -> Result<()> {
    if !(200..300).contains(&response.status) {
        return Err(NotifyError::Send(format!(
            "webhook request failed with status {}: {}",
            response.status,
            response.body_text()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escaping_uses_go_unicode_forms() {
        assert_eq!(
            json_unicode_escape_string("a\"b\\c\n\t"),
            "a\\u0022b\\u005Cc\\u000A\\u0009"
        );
        assert_eq!(json_unicode_escape_string("\u{1}"), "\\u0001");
    }
}
