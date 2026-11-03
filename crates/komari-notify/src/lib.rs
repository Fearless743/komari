//! Notification delivery and notification decision logic.
//!
//! This crate mirrors the Go backend's `utils/messageSender` and
//! `utils/notifier` packages. The HTTP abstraction and provider factory are
//! added first; providers, the retry service and the notifier rules build on
//! top of them.

pub mod error;
pub mod http;
pub mod provider;

pub use error::{NotifyError, Result};
pub use http::{
    HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, form_encode, query_escape,
};
pub use provider::{ConfigField, EventMessageSender, MessageSender};
