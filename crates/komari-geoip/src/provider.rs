//! Provider abstraction shared by all GeoIP backends.

use std::net::IpAddr;

use async_trait::async_trait;

use crate::error::GeoIpError;
use crate::model::GeoInfo;

/// A GeoIP backend, mirroring Go's `geoip.GeoIPService`.
#[async_trait]
pub trait GeoIpProvider: Send + Sync {
    /// Display name, matching the Go `Name()` implementations (`ip-api.com`,
    /// `ipinfo.io`, `geojs.io`, `MaxMind`, `EmptyProvider`).
    fn name(&self) -> &str;

    /// Look up one address. A provider without data for an address returns an
    /// empty [`GeoInfo`] rather than failing.
    async fn get_geo_info(&self, ip: IpAddr) -> Result<GeoInfo, GeoIpError>;

    /// Refresh a local database. Network providers are no-ops. This always
    /// performs the refresh (download for `mmdb`); use
    /// [`GeoIpProvider::prepare`] during initialization instead.
    async fn update_database(&self) -> Result<(), GeoIpError> {
        Ok(())
    }

    /// Prepare local storage before the provider is installed.
    ///
    /// Network providers do nothing. `mmdb` loads an existing database or
    /// downloads a missing one, without forcing a refresh when the file is
    /// already present.
    async fn prepare(&self) -> Result<(), GeoIpError> {
        Ok(())
    }

    /// Release resources. Network providers are no-ops.
    async fn close(&self) -> Result<(), GeoIpError> {
        Ok(())
    }
}
