use serde::{Deserialize, Serialize};

use super::{Client, FromRow};
use crate::json::StringArray;
use crate::time::LocalTime;

/// Offline-alert settings for one client (`offline_notifications`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OfflineNotification {
    pub client: String,
    /// Joined client information. Not a column on `offline_notifications`.
    ///
    /// Go declares this as a value type (`Client`, not `*Client`), so it is
    /// always present in JSON even when it is the zero value.
    #[serde(deserialize_with = "deserialize_client_or_default")]
    pub client_info: Client,
    pub enable: bool,
    pub grace_period: i64,
    pub last_notified: LocalTime,
}

impl OfflineNotification {
    pub fn new(client: impl Into<String>) -> Self {
        Self {
            client: client.into(),
            enable: false,
            grace_period: 180,
            last_notified: LocalTime::default(),
            client_info: Client::default(),
        }
    }
}

/// `OfflineNotification.client_info` is a value type in Go; JSON `null` leaves
/// it at the zero value instead of failing deserialization.
fn deserialize_client_or_default<'de, D>(deserializer: D) -> Result<Client, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Client>::deserialize(deserializer)?.unwrap_or_default())
}

impl FromRow for OfflineNotification {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            client: row.get("client")?,
            client_info: Client::default(),
            enable: row.get("enable")?,
            grace_period: row.get("grace_period")?,
            last_notified: row.get("last_notified")?,
        })
    }
}

/// A load-based notification rule (`load_notifications`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LoadNotification {
    #[serde(skip_serializing_if = "crate::models::is_zero_i64")]
    pub id: i64,
    pub name: String,
    pub clients: StringArray,
    pub metric: String,
    pub threshold: f32,
    pub ratio: f32,
    pub interval: i64,
    pub last_notified: LocalTime,
}

impl LoadNotification {
    pub fn new(name: impl Into<String>, metric: impl Into<String>) -> Self {
        Self {
            id: 0,
            name: name.into(),
            clients: StringArray::default(),
            metric: metric.into(),
            threshold: 80.0,
            ratio: 0.8,
            interval: 15,
            last_notified: LocalTime::default(),
        }
    }
}

impl FromRow for LoadNotification {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            clients: row.get("clients")?,
            metric: row.get("metric")?,
            threshold: row.get("threshold")?,
            ratio: row.get("ratio")?,
            interval: row.get("interval")?,
            last_notified: row.get("last_notified")?,
        })
    }
}
