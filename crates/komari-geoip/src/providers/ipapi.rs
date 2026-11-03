//! ip-api.com GeoIP provider.

use std::net::IpAddr;

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::GeoIpError;
use crate::factory::GeoIpFactory;
use crate::model::GeoInfo;
use crate::provider::GeoIpProvider;

/// Default endpoint, matching the Go provider.
pub const IPAPI_BASE_URL: &str = "http://ip-api.com/json";

/// ip-api.com provider.
pub struct IpApiProvider {
    client: reqwest::Client,
    base_url: String,
}

impl Default for IpApiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl IpApiProvider {
    pub fn new() -> Self {
        Self::with_base_url(IPAPI_BASE_URL)
    }

    /// Create a provider that talks to another origin (used by tests).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("building the ip-api HTTP client cannot fail");
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
        }
    }
}

#[async_trait]
impl GeoIpProvider for IpApiProvider {
    fn name(&self) -> &str {
        "ip-api.com"
    }

    async fn get_geo_info(&self, ip: IpAddr) -> Result<GeoInfo, GeoIpError> {
        // The field list matches the Go implementation exactly.
        let url = format!(
            "{}/{}?fields=status,message,country,countryCode",
            self.base_url, ip
        );
        let response = self.client.get(url).send().await?;
        let body = response.text().await.unwrap_or_default();
        let api: IpApiResponse = serde_json::from_str(&body).map_err(|error| {
            GeoIpError::Provider(format!("failed to decode ip-api.com response: {error}"))
        })?;
        if api.status != "success" {
            return Err(GeoIpError::Provider(format!(
                "ip-api.com returned an error: {}",
                api.message
            )));
        }
        Ok(GeoInfo {
            iso_code: api.country_code,
            name: api.country.clone(),
            country: non_empty(api.country),
            province: non_empty(api.region_name),
            city: non_empty(api.city),
            isp: non_empty(api.isp),
            latitude: api.lat,
            longitude: api.lon,
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
struct IpApiResponse {
    #[serde(default)]
    status: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    country: String,
    #[serde(default, rename = "countryCode")]
    country_code: String,
    #[serde(default, rename = "regionName")]
    region_name: String,
    #[serde(default)]
    city: String,
    #[serde(default)]
    isp: String,
    #[serde(default)]
    lat: Option<f64>,
    #[serde(default)]
    lon: Option<f64>,
}

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Register the `ip-api` provider.
pub fn register(factory: &GeoIpFactory) {
    factory.register_provider(
        "ip-api",
        std::sync::Arc::new(|| Ok(Box::new(IpApiProvider::new()) as Box<_>)),
    );
}
