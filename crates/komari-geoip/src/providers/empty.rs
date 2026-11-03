//! Empty GeoIP provider used when GeoIP is disabled or misconfigured.

use std::net::IpAddr;

use async_trait::async_trait;

use crate::error::GeoIpError;
use crate::factory::GeoIpFactory;
use crate::model::GeoInfo;
use crate::provider::GeoIpProvider;

/// A provider that reports that no lookup backend is available.
#[derive(Clone, Debug, Default)]
pub struct EmptyProvider;

#[async_trait]
impl GeoIpProvider for EmptyProvider {
    fn name(&self) -> &str {
        "EmptyProvider"
    }

    async fn get_geo_info(&self, _ip: IpAddr) -> Result<GeoInfo, GeoIpError> {
        Err(GeoIpError::EmptyProvider)
    }

    async fn update_database(&self) -> Result<(), GeoIpError> {
        Err(GeoIpError::EmptyProvider)
    }
}

/// Register the `empty` provider.
pub fn register(factory: &GeoIpFactory) {
    factory.register_provider("empty", std::sync::Arc::new(|| Ok(Box::new(EmptyProvider))));
}
