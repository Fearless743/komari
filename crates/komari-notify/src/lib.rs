//! Notification delivery and notification decision logic.
//!
//! This crate mirrors the Go backend's `utils/messageSender` and
//! `utils/notifier` packages.

pub mod error;
pub mod http;
pub mod provider;
pub mod providers;
pub mod registry;

pub use error::{NotifyError, Result};
pub use http::{
    HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, form_encode, query_escape,
};
pub use provider::{ConfigField, EventMessageSender, MessageSender};
pub use registry::{
    ProviderRegistration, ProviderRegistry, get_provider_configuration, initialize_defaults,
    load_provider, load_provider_from_db, registry,
};
