//! Cloudflare DNS provider (API v4).
//!
//! Behaviour follows `utils/ddns/cloudflare/cloudflare.go`: the record is
//! looked up by hostname/type when no record id is known, created when the
//! lookup finds nothing, and patched otherwise.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::DdnsError;
use crate::factory::DdnsFactory;
use crate::provider::{ConfigurationItem, DdnsProvider, SyncContext, SyncResult, config_string};

/// Default Cloudflare API base URL, overridable in tests.
pub const CLOUDFLARE_API_BASE_URL: &str = "https://api.cloudflare.com/client/v4";

/// Configuration fields that must never be returned by public APIs.
pub const CLOUDFLARE_SENSITIVE_FIELDS: &[&str] = &["api_token", "zone_id"];

/// Configuration stored in `ddns_providers.addition` for Cloudflare.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudflareConfig {
    pub api_token: String,
    pub zone_id: String,
    pub record_id: String,
    pub record_type: String,
    pub ttl: i64,
    pub proxied: bool,
}

impl std::fmt::Debug for CloudflareConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CloudflareConfig")
            .field("api_token", &"<redacted>")
            .field("zone_id", &"<redacted>")
            .field("record_id", &self.record_id)
            .field("record_type", &self.record_type)
            .field("ttl", &self.ttl)
            .field("proxied", &self.proxied)
            .finish()
    }
}

/// Cloudflare DDNS provider.
pub struct CloudflareProvider {
    config: CloudflareConfig,
    base_url: String,
    client: reqwest::Client,
}

impl Default for CloudflareProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CloudflareProvider {
    pub fn new() -> Self {
        Self::with_base_url(CLOUDFLARE_API_BASE_URL)
    }

    /// Create a provider that talks to another API origin (used by tests).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("building the Cloudflare HTTP client cannot fail");
        Self {
            config: CloudflareConfig::default(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            client,
        }
    }

    pub fn config(&self) -> &CloudflareConfig {
        &self.config
    }
}

#[async_trait]
impl DdnsProvider for CloudflareProvider {
    fn name(&self) -> &str {
        "cloudflare"
    }

    fn configuration(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    fn set_configuration(&mut self, configuration: Value) -> Result<(), DdnsError> {
        if configuration.is_null() {
            return Ok(());
        }
        self.config = serde_json::from_value(configuration)
            .map_err(|_| DdnsError::InvalidConfig("invalid cloudflare configuration".to_owned()))?;
        Ok(())
    }

    fn configuration_items(&self) -> Vec<ConfigurationItem> {
        vec![
            ConfigurationItem::new("api_token").required(),
            ConfigurationItem::new("zone_id").required(),
            ConfigurationItem::new("record_id").help("Optional global fallback DNS record ID"),
            ConfigurationItem::new("record_type")
                .item_type("option")
                .options("A,AAAA")
                .default_value("A"),
            ConfigurationItem::new("ttl")
                .item_type("int")
                .default_value("1")
                .help("1 means automatic in Cloudflare"),
            ConfigurationItem::new("proxied")
                .item_type("bool")
                .default_value("false"),
        ]
    }

    fn sensitive_fields(&self) -> &'static [&'static str] {
        CLOUDFLARE_SENSITIVE_FIELDS
    }

    async fn sync(&self, context: &SyncContext) -> Result<SyncResult, DdnsError> {
        if self.config.api_token.trim().is_empty() || self.config.zone_id.trim().is_empty() {
            return Err(DdnsError::NotConfigured(
                "cloudflare DDNS is not fully configured".to_owned(),
            ));
        }

        let record_type = self.record_type(context);
        let content = match record_type.as_str() {
            "A" => context.ipv4.trim(),
            "AAAA" => context.ipv6.trim(),
            other => {
                return Err(DdnsError::Provider(format!(
                    "unsupported record type: {other}"
                )));
            }
        };
        if content.is_empty() {
            return Err(DdnsError::Provider(format!(
                "no IP available for record type {record_type}"
            )));
        }

        let hostname = config_string(&context.provider_config, "hostname")
            .trim()
            .to_owned();
        let mut payload = Map::new();
        payload.insert("type".to_owned(), json!(record_type));
        payload.insert("content".to_owned(), json!(content));
        payload.insert("ttl".to_owned(), json!(self.config.ttl));
        payload.insert("proxied".to_owned(), json!(self.config.proxied));
        payload.insert(
            "comment".to_owned(),
            json!(format!("updated by Komari for {}", context.client_uuid)),
        );
        if !hostname.is_empty() {
            payload.insert("name".to_owned(), json!(hostname));
        }

        let mut record_id = self.config.record_id.trim().to_owned();
        let context_record_id = config_string(&context.provider_config, "record_id");
        if !context_record_id.trim().is_empty() {
            record_id = context_record_id.trim().to_owned();
        }

        if record_id.is_empty() {
            if hostname.is_empty() {
                return Err(DdnsError::Provider(
                    "cloudflare record id is not configured and hostname is empty".to_owned(),
                ));
            }
            match self.lookup_record_id(&hostname, &record_type).await {
                Ok(found) => record_id = found,
                Err(error) if error.is_record_not_found() => {
                    let created = self.create_record(&Value::Object(payload)).await?;
                    return Ok(SyncResult::resolved(created));
                }
                Err(error) => return Err(error),
            }
        }

        self.update_record(&record_id, &Value::Object(payload))
            .await?;
        Ok(SyncResult::resolved(record_id))
    }

    async fn delete_record(&self, record_id: &str) -> Result<(), DdnsError> {
        if self.config.api_token.trim().is_empty() || self.config.zone_id.trim().is_empty() {
            return Err(DdnsError::NotConfigured(
                "cloudflare DDNS is not fully configured".to_owned(),
            ));
        }
        let request = self.client.delete(format!(
            "{}/zones/{}/dns_records/{}",
            self.base_url,
            self.config.zone_id,
            record_id.trim()
        ));
        let (_, body) = self
            .send(self.headers(request), "cloudflare delete")
            .await?;

        let response: CloudflareMutationResponse = serde_json::from_str(&body)?;
        if !response.success {
            return Err(DdnsError::Provider(format!(
                "cloudflare delete error: {}",
                error_messages(&response.errors)
            )));
        }
        Ok(())
    }
}

impl CloudflareProvider {
    fn record_type(&self, context: &SyncContext) -> String {
        let mut record_type = self.config.record_type.trim().to_ascii_uppercase();
        if record_type.is_empty() {
            record_type = "A".to_owned();
        }
        let inherited = config_string(&context.provider_config, "record_type");
        let inherited = inherited.trim().to_ascii_uppercase();
        if !inherited.is_empty() && inherited != "INHERIT" {
            record_type = inherited;
        }
        record_type
    }

    fn headers(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .bearer_auth(&self.config.api_token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        service: &'static str,
    ) -> Result<(reqwest::StatusCode, String), DdnsError> {
        let response = request.send().await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(DdnsError::HttpStatus {
                service,
                status: status.as_u16(),
                body,
            });
        }
        Ok((status, body))
    }

    async fn lookup_record_id(
        &self,
        hostname: &str,
        record_type: &str,
    ) -> Result<String, DdnsError> {
        let request = self.client.get(format!(
            "{}/zones/{}/dns_records",
            self.base_url, self.config.zone_id
        ));
        let request = self.headers(request).query(&[
            ("type", record_type),
            ("name", hostname),
            ("per_page", "10"),
        ]);
        let (_, body) = self.send(request, "cloudflare lookup").await?;

        #[derive(Deserialize)]
        struct LookupResponse {
            #[serde(default)]
            success: bool,
            #[serde(default)]
            errors: Vec<CloudflareMessage>,
            #[serde(default)]
            result: Vec<CloudflareRecord>,
        }
        let response: LookupResponse = serde_json::from_str(&body)?;
        if !response.success {
            return Err(DdnsError::Provider(format!(
                "cloudflare lookup error: {}",
                error_messages(&response.errors)
            )));
        }
        if response.result.is_empty() {
            return Err(DdnsError::RecordNotFound(format!(
                "hostname={hostname} type={record_type}"
            )));
        }
        Ok(response
            .result
            .iter()
            .find(|record| record.comment.contains("updated by Komari"))
            .unwrap_or(&response.result[0])
            .id
            .clone())
    }

    async fn create_record(&self, payload: &Value) -> Result<String, DdnsError> {
        let request = self
            .client
            .post(format!(
                "{}/zones/{}/dns_records",
                self.base_url, self.config.zone_id
            ))
            .json(payload);
        let (_, body) = self
            .send(self.headers(request), "cloudflare create")
            .await?;

        #[derive(Deserialize)]
        struct CreateResponse {
            #[serde(default)]
            success: bool,
            #[serde(default)]
            errors: Vec<CloudflareMessage>,
            #[serde(default)]
            result: Option<CloudflareRecord>,
        }
        let response: CreateResponse = serde_json::from_str(&body)?;
        if !response.success {
            return Err(DdnsError::Provider(format!(
                "cloudflare create error: {}",
                error_messages(&response.errors)
            )));
        }
        let record_id = response.result.map(|record| record.id).unwrap_or_default();
        let record_id = record_id.trim().to_owned();
        if record_id.is_empty() {
            return Err(DdnsError::Provider(
                "cloudflare create returned empty record id".to_owned(),
            ));
        }
        Ok(record_id)
    }

    async fn update_record(&self, record_id: &str, payload: &Value) -> Result<(), DdnsError> {
        let request = self
            .client
            .patch(format!(
                "{}/zones/{}/dns_records/{}",
                self.base_url, self.config.zone_id, record_id
            ))
            .json(payload);
        let (_, body) = self
            .send(self.headers(request), "cloudflare update")
            .await?;

        let response: CloudflareMutationResponse = serde_json::from_str(&body)?;
        if !response.success {
            return Err(DdnsError::Provider(format!(
                "cloudflare api returned error: {}",
                error_messages(&response.errors)
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
struct CloudflareMutationResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    errors: Vec<CloudflareMessage>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct CloudflareMessage {
    #[serde(default)]
    message: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct CloudflareRecord {
    #[serde(default)]
    id: String,
    #[serde(default)]
    comment: String,
}

fn error_messages(errors: &[CloudflareMessage]) -> String {
    if errors.is_empty() {
        return "unknown error".to_owned();
    }
    errors
        .iter()
        .map(|error| error.message.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Register the `cloudflare` provider.
pub fn register(factory: &DdnsFactory) {
    factory.register_provider(std::sync::Arc::new(|| Box::new(CloudflareProvider::new())));
}
