//! Notification delivery and notification decision logic.
//!
//! * [`provider`] / [`registry`] mirror Go's message-sender factory.
//! * [`providers`] contains the individual senders.
//! * [`event`] / [`service`] implement templating and retries.
//! * [`notifier`] ports offline, traffic, expiry and load rules.

pub mod error;
pub mod event;
pub mod http;
pub mod notifier;
pub mod provider;
pub mod providers;
pub mod redact;
pub mod registry;
pub mod service;

pub use error::{NotifyError, Result};
pub use event::{DEFAULT_TEMPLATE, format_event};
pub use http::{
    HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, form_encode, query_escape,
};
pub use provider::{ConfigField, EventMessageSender, MessageSender};
pub use redact::redact_addition;
pub use registry::{
    ProviderRegistration, ProviderRegistry, get_provider_configuration, initialize_defaults,
    load_provider, load_provider_from_db, registry,
};
pub use service::{NotificationService, NotificationSettings};
