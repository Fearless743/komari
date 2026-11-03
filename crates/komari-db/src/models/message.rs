use serde::{Deserialize, Serialize};

use super::Client;
use crate::time::LocalTime;

/// Event types used by the notification subsystem
/// (`database/models/messageEvent`).
pub mod message_event {
    pub const OFFLINE: &str = "Offline";
    pub const ONLINE: &str = "Online";
    pub const EXPIRE: &str = "Expire";
    pub const RENEW: &str = "Renew";
    pub const LOGIN: &str = "Login";
    pub const ALERT: &str = "Alert";
    pub const TRAFFIC: &str = "Traffic";
}

/// An event delivered to a notification sender. Not a database table.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EventMessage {
    pub event: String,
    pub clients: Vec<Client>,
    pub time: LocalTime,
    pub message: String,
    pub emoji: String,
}
