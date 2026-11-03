//! ipinfo.io GeoIP provider.

use std::net::IpAddr;

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::GeoIpError;
use crate::factory::GeoIpFactory;
use crate::model::GeoInfo;
use crate::provider::GeoIpProvider;

/// Default endpoint, matching the Go provider.
pub const IPINFO_BASE_URL: &str = "https://ipinfo.io";

/// ipinfo.io provider.
pub struct IpInfoProvider {
    client: reqwest::Client,
    base_url: String,
}

impl Default for IpInfoProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl IpInfoProvider {
    pub fn new() -> Self {
        Self::with_base_url(IPINFO_BASE_URL)
    }

    /// Create a provider that talks to another origin (used by tests).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("building the ipinfo HTTP client cannot fail");
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
        }
    }
}

#[async_trait]
impl GeoIpProvider for IpInfoProvider {
    fn name(&self) -> &str {
        "ipinfo.io"
    }

    async fn get_geo_info(&self, ip: IpAddr) -> Result<GeoInfo, GeoIpError> {
        let url = format!("{}/{}/json", self.base_url, ip);
        let response = self.client.get(url).send().await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status.as_u16() != 200 {
            return Err(GeoIpError::HttpStatus {
                service: "ipinfo.io",
                status: status.as_u16(),
                body,
            });
        }
        let api: IpInfoResponse = serde_json::from_str(&body).map_err(|error| {
            GeoIpError::Provider(format!("failed to decode ipinfo.io response: {error}"))
        })?;
        let (latitude, longitude) = parse_location(&api.loc);
        Ok(GeoInfo {
            // ipinfo.io's free tier returns the ISO code in `country`.
            iso_code: api.country.clone(),
            name: api.country.clone(),
            country: non_empty(api.country),
            province: non_empty(api.region),
            city: non_empty(api.city),
            isp: non_empty(api.org),
            latitude,
            longitude,
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[allow(dead_code)]
struct IpInfoResponse {
    #[serde(default)]
    ip: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    city: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    country: String,
    #[serde(default, rename = "countryCode")]
    country_code: String,
    #[serde(default)]
    loc: String,
    #[serde(default)]
    org: String,
    #[serde(default)]
    postal: String,
    #[serde(default)]
    timezone: String,
}

fn parse_location(location: &str) -> (Option<f64>, Option<f64>) {
    let Some((latitude, longitude)) = location.split_once(',') else {
        return (None, None);
    };
    (latitude.trim().parse().ok(), longitude.trim().parse().ok())
}

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Register the `ipinfo` provider.
pub fn register(factory: &GeoIpFactory) {
    factory.register_provider(
        "ipinfo",
        std::sync::Arc::new(|| Ok(Box::new(IpInfoProvider::new()) as Box<_>)),
    );
}
