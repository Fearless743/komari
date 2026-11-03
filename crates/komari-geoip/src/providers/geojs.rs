//! geojs.io GeoIP provider.

use std::net::IpAddr;

use async_trait::async_trait;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::error::GeoIpError;
use crate::factory::GeoIpFactory;
use crate::model::GeoInfo;
use crate::provider::GeoIpProvider;

/// Default endpoint, matching the Go provider.
pub const GEOJS_BASE_URL: &str = "https://get.geojs.io/v1/ip/geo";

/// geojs.io provider.
pub struct GeoJsProvider {
    client: reqwest::Client,
    base_url: String,
}

impl Default for GeoJsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GeoJsProvider {
    pub fn new() -> Self {
        Self::with_base_url(GEOJS_BASE_URL)
    }

    /// Create a provider that talks to another origin (used by tests).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("building the geojs HTTP client cannot fail");
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
        }
    }
}

#[async_trait]
impl GeoIpProvider for GeoJsProvider {
    fn name(&self) -> &str {
        "geojs.io"
    }

    async fn get_geo_info(&self, ip: IpAddr) -> Result<GeoInfo, GeoIpError> {
        let url = format!("{}/{}.json", self.base_url, ip);
        let response = self.client.get(url).send().await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status.as_u16() != 200 {
            return Err(GeoIpError::HttpStatus {
                service: "geojs.io",
                status: status.as_u16(),
                body,
            });
        }
        let api: GeoJsResponse = serde_json::from_str(&body).map_err(|error| {
            GeoIpError::Provider(format!("failed to decode geojs.io response: {error}"))
        })?;
        // geojs returns 200 with an empty body for private/invalid addresses.
        if api.country_code.is_empty() {
            return Err(GeoIpError::Provider(format!(
                "geojs.io returned empty geo info for ip: {ip}"
            )));
        }
        let isp = if api.organization_name.is_empty() {
            api.organization
        } else {
            api.organization_name
        };
        Ok(GeoInfo {
            iso_code: api.country_code,
            name: api.country.clone(),
            country: non_empty(api.country),
            province: non_empty(api.region),
            city: non_empty(api.city),
            isp: non_empty(isp),
            latitude: api.latitude,
            longitude: api.longitude,
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
struct GeoJsResponse {
    #[serde(default)]
    country: String,
    #[serde(default)]
    country_code: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    city: String,
    #[serde(default)]
    organization_name: String,
    #[serde(default)]
    organization: String,
    #[serde(default, deserialize_with = "deserialize_optional_f64")]
    latitude: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_f64")]
    longitude: Option<f64>,
}

/// geojs returns coordinates as strings (`"37.4"`), while other services use
/// JSON numbers. Accept both.
fn deserialize_optional_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        None | Some(Value::Null) => None,
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.trim().parse().ok(),
        _ => None,
    })
}

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Register the `geojs` provider.
pub fn register(factory: &GeoIpFactory) {
    factory.register_provider(
        "geojs",
        std::sync::Arc::new(|| Ok(Box::new(GeoJsProvider::new()) as Box<_>)),
    );
}
