//! Database models mirroring `komari/database/models/**`.
//!
//! Table names, column names and JSON field names follow GORM's defaults
//! (plural snake_case table names, snake_case columns). Models deliberately
//! mirror the Go structs field-for-field so an existing `data/komari.db` can be
//! read and written without a conversion step.

mod client;
pub mod clipboard;
pub mod config;
pub mod ddns;
pub mod log;
pub mod message;
pub mod notification;
pub mod provider;
pub mod record;
pub mod session;
pub mod theme;
pub mod user;

pub use clipboard::Clipboard;
pub use config::{Config, ConfigItem};
pub use ddns::DdnsSyncHistory;
pub use log::Log;
pub use message::{EventMessage, message_event};
pub use notification::{LoadNotification, OfflineNotification};
pub use provider::{DdnsProvider, MessageSenderProvider, OidcProvider, Provider};
pub use record::{GPURecord, Record};
pub use session::Session;
pub use theme::{Configuration, ManagedThemeConfigurationItem, Theme, ThemeConfiguration};
pub use user::User;

pub use client::Client;

/// Implemented by models that can be built from a SQLite row using column names.
pub trait FromRow: Sized {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self>;
}
