//! DDNS providers and synchronization for Komari.
//!
//! This crate mirrors the Go packages under `utils/ddns`:
//!
//! | Go | Rust |
//! | --- | --- |
//! | `factory.IDdnsProvider` | [`provider::DdnsProvider`] |
//! | `factory.RegisterDdnsProvider` | [`factory::DdnsFactory::register_provider`] |
//! | `factory.GetProviderConfigs` | [`factory::DdnsFactory::get_provider_configs`] |
//! | `cloudflare` / `webhook` / `empty` | [`providers`] |
//! | `ddns.LoadProvider` / `SyncClient` | [`DdnsService`] |
//!
//! # Example
//!
//! ```
//! use komari_ddns::factory::DdnsFactory;
//!
//! let factory = DdnsFactory::with_builtins();
//! assert!(factory.get_constructor("cloudflare").is_ok());
//! ```

pub mod error;
pub mod factory;
pub mod provider;
pub mod providers;
pub mod service;

pub use error::DdnsError;
pub use factory::{DdnsConstructor, DdnsFactory, factory};
pub use provider::{
    ConfigurationItem, DdnsProvider, ProviderConfig, SyncContext, SyncResult, config_string,
};
pub use service::{DdnsService, merge_client_provider_config};
