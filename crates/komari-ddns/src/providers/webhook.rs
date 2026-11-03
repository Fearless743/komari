//! Generic webhook DDNS provider.
//!
//! The request body is a template with the same placeholders as
//! `utils/ddns/webhook/webhook.go`, and an optional dotted JSON path can
//! extract a record id from the response.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::DdnsError;
use crate::factory::DdnsFactory;
use crate::provider::{ConfigurationItem, DdnsProvider, SyncContext, SyncResult, config_string};

/// Body template offered by the admin UI when the field is first rendered.
pub const DEFAULT_BODY: &str = r#"{"client_uuid":"{{client_uuid}}","client_name":"{{client_name}}","ipv4":"{{ipv4}}","ipv6":"{{ipv6}}","triggered_by":"{{triggered_by}}"}"#;

/// Configuration fields that must never be returned by public APIs. The URL
/// is additionally removed by the redaction helper when it embeds credentials.
pub const WEBHOOK_SENSITIVE_FIELDS: &[&str] = &["headers", "body"];

/// Configuration stored in `ddns_providers.addition` for a webhook.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebhookConfig {
    pub url: String,
    pub method: String,
    pub content_type: String,
    pub headers: String,
    pub body: String,
    pub response_json_path: String,
}

impl std::fmt::Debug for WebhookConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let url = if crate::redaction::url_contains_credentials(&self.url) {
            "<redacted>"
        } else {
            self.url.as_str()
        };
        formatter
            .debug_struct("WebhookConfig")
            .field("url", &url)
            .field("method", &self.method)
            .field("content_type", &self.content_type)
            .field("headers", &"<redacted>")
            .field("body", &"<redacted>")
            .field("response_json_path", &self.response_json_path)
            .finish()
    }
}

/// Webhook DDNS provider.
pub struct WebhookProvider {
    config: WebhookConfig,
    client: reqwest::Client,
}

impl Default for WebhookProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl WebhookProvider {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("building the webhook HTTP client cannot fail");
        Self {
            config: WebhookConfig::default(),
            client,
        }
    }

    pub fn config(&self) -> &WebhookConfig {
        &self.config
    }
}

#[async_trait]
impl DdnsProvider for WebhookProvider {
    fn name(&self) -> &str {
        "webhook"
    }

    fn configuration(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    fn set_configuration(&mut self, configuration: Value) -> Result<(), DdnsError> {
        if configuration.is_null() {
            return Ok(());
        }
        self.config = serde_json::from_value(configuration)
            .map_err(|error| DdnsError::InvalidConfig(error.to_string()))?;
        Ok(())
    }

    fn configuration_items(&self) -> Vec<ConfigurationItem> {
        vec![
            ConfigurationItem::new("url").required(),
            ConfigurationItem::new("method")
                .item_type("option")
                .options("POST,PUT,PATCH")
                .default_value("POST"),
            ConfigurationItem::new("content_type").default_value("application/json"),
            ConfigurationItem::new("headers").help("HTTP headers in JSON format"),
            ConfigurationItem::new("body")
                .item_type("richtext")
                .default_value(DEFAULT_BODY),
            ConfigurationItem::new("response_json_path")
                .help("JSON path to extract record_id from response, e.g. data.record_id"),
        ]
    }

    fn sensitive_fields(&self) -> &'static [&'static str] {
        WEBHOOK_SENSITIVE_FIELDS
    }

    async fn sync(&self, context: &SyncContext) -> Result<SyncResult, DdnsError> {
        if self.config.url.trim().is_empty() {
            return Err(DdnsError::NotConfigured(
                "webhook URL is not configured".to_owned(),
            ));
        }

        let method = self.config.method.trim().to_ascii_uppercase();
        let method = if method.is_empty() {
            reqwest::Method::POST
        } else {
            reqwest::Method::from_bytes(method.as_bytes()).map_err(|error| {
                DdnsError::InvalidConfig(format!("invalid webhook method: {error}"))
            })?
        };

        let content_type = self.config.content_type.trim();
        let content_type = if content_type.is_empty() {
            "application/json"
        } else {
            content_type
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_str(content_type).map_err(|error| {
                DdnsError::InvalidConfig(format!("invalid content type: {error}"))
            })?,
        );
        if !self.config.headers.trim().is_empty() {
            let custom: std::collections::HashMap<String, String> =
                serde_json::from_str(&self.config.headers).map_err(|error| {
                    DdnsError::InvalidConfig(format!("invalid headers json: {error}"))
                })?;
            for (name, value) in custom {
                let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
                    DdnsError::InvalidConfig(format!("invalid header name: {error}"))
                })?;
                let value = HeaderValue::from_str(&value).map_err(|error| {
                    DdnsError::InvalidConfig(format!("invalid header value: {error}"))
                })?;
                headers.insert(name, value);
            }
        }

        let body = replace_placeholders(&self.config.body, context);
        let response = self
            .client
            .request(method, &self.config.url)
            .headers(headers)
            .body(body)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(DdnsError::HttpStatus {
                service: "webhook",
                status: status.as_u16(),
                body,
            });
        }

        let mut result = SyncResult::default();
        if !self.config.response_json_path.is_empty() {
            let record_id = extract_json_path(&body, &self.config.response_json_path);
            if !record_id.is_empty() {
                result.resolved_record_id = record_id;
            }
        }
        Ok(result)
    }
}

/// Replace `{{name}}` placeholders in one pass, mirroring `strings.Replacer`.
pub fn replace_placeholders(template: &str, context: &SyncContext) -> String {
    let hostname = config_string(&context.provider_config, "hostname");
    let record_id = config_string(&context.provider_config, "record_id");
    let record_type = config_string(&context.provider_config, "record_type");
    let replacements = [
        ("{{ipv4}}", context.ipv4.as_str()),
        ("{{ipv6}}", context.ipv6.as_str()),
        ("{{client_uuid}}", context.client_uuid.as_str()),
        ("{{client_name}}", context.client_name.as_str()),
        ("{{triggered_by}}", context.triggered_by.as_str()),
        ("{{hostname}}", hostname.as_str()),
        ("{{record_id}}", record_id.as_str()),
        ("{{record_type}}", record_type.as_str()),
    ];

    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while !rest.is_empty() {
        let next = replacements
            .iter()
            .filter_map(|(needle, value)| rest.find(needle).map(|index| (index, *needle, *value)))
            .min_by_key(|(index, _, _)| *index);
        match next {
            Some((index, needle, value)) => {
                output.push_str(&rest[..index]);
                output.push_str(value);
                rest = &rest[index + needle.len()..];
            }
            None => {
                output.push_str(rest);
                break;
            }
        }
    }
    output
}

/// Extract a string from a dotted JSON path. Numeric segments index arrays,
/// matching the Go helper.
pub fn extract_json_path(body: &str, path: &str) -> String {
    let Ok(mut current) = serde_json::from_str::<Value>(body) else {
        return String::new();
    };
    for key in path.split('.') {
        current = match current {
            Value::Object(ref mut map) => match map.get(key) {
                Some(value) => value.clone(),
                None => return String::new(),
            },
            Value::Array(ref array) => {
                let Ok(index) = key.parse::<usize>() else {
                    return String::new();
                };
                match array.get(index) {
                    Some(value) => value.clone(),
                    None => return String::new(),
                }
            }
            _ => return String::new(),
        };
    }
    match current {
        Value::String(text) => text,
        _ => String::new(),
    }
}

/// Register the `webhook` provider.
pub fn register(factory: &DdnsFactory) {
    factory.register_provider(std::sync::Arc::new(|| Box::new(WebhookProvider::new())));
}
