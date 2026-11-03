//! GeoIP lookups for Komari.
//!
//! This crate mirrors the Go packages under `utils/geoip`:
//!
//! | Go | Rust |
//! | --- | --- |
//! | `geoip.GeoIPService` | [`provider::GeoIpProvider`] |
//! | `EmptyProvider` | [`providers::empty::EmptyProvider`] |
//! | `MaxMindGeoIPService` | [`providers::mmdb::MaxMindProvider`] |
//! | `IPAPIService` / `IPInfoService` / `GeoJSService` | [`providers`] |
//! | `InitGeoIp` / `GetGeoInfo` | [`GeoIpService`] |
//!
//! Providers never panic on missing configuration or missing data: the
//! service falls back to an empty [`GeoInfo`].

pub mod error;
pub mod factory;
pub mod model;
pub mod provider;
pub mod providers;
pub mod service;

pub use error::GeoIpError;
pub use factory::{GeoIpConstructor, GeoIpFactory, factory};
pub use model::{GeoInfo, region_unicode_emoji};
pub use provider::GeoIpProvider;
pub use service::GeoIpService;
