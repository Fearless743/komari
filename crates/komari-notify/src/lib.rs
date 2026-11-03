//! Notification delivery and notification decision logic.
//!
//! This crate mirrors the Go backend's `utils/messageSender` and
//! `utils/notifier` packages.

pub mod error;
pub mod http;
pub mod provider;
pub mod providers;

pub use error::{NotifyError, Result};
pub use http::{
    HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, form_encode, query_escape,
};
pub use provider::{ConfigField, EventMessageSender, MessageSender};
