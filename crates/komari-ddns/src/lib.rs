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
//! | (security deviation) raw admin `addition` | [`redaction::redact_addition`] |
//!
//! # Example
//!
//! ```
//! use komari_ddns::factory::DdnsFactory;
//!
//! let factory = DdnsFactory::with_builtins();
//! assert!(factory.get_constructor("cloudflare").is_ok());
//! ```
//!
//! Public API responses should redact stored provider configurations:
//!
//! ```
//! use komari_ddns::redact_addition;
//!
//! let safe = redact_addition(
//!     "cloudflare",
//!     r#"{"api_token":"secret","zone_id":"zone","record_id":"record-1"}"#,
//! );
//! assert!(safe.get("api_token").is_none());
//! assert_eq!(safe["record_id"], "record-1");
//! ```

pub mod error;
pub mod factory;
pub mod provider;
pub mod providers;
pub mod redaction;
pub mod service;

pub use error::DdnsError;
pub use factory::{DdnsConstructor, DdnsFactory, factory};
pub use provider::{
    ConfigurationItem, DdnsProvider, ProviderConfig, SyncContext, SyncResult, config_string,
};
pub use redaction::{redact_addition, redact_addition_with_fields, redact_provider_configuration};
pub use service::{DdnsService, merge_client_provider_config};
