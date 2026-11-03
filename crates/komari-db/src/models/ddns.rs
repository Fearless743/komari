use serde::{Deserialize, Serialize};

use super::FromRow;
use crate::time::LocalTime;

/// A DDNS synchronization attempt (`ddns_sync_history`).
///
/// Note the column names `ip_v4` and `ip_v6`: GORM derives them from the Go
/// fields `IPV4` / `IPV6`, not from their JSON tags.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DdnsSyncHistory {
    pub id: i64,
    pub client_uuid: String,
    pub client_name: String,
    pub hostname: String,
    pub record_type: String,
    #[serde(rename = "ipv4")]
    pub ipv4: String,
    #[serde(rename = "ipv6")]
    pub ipv6: String,
    pub record_id: String,
    pub status: String,
    pub error: String,
    pub triggered_by: String,
    pub synced_at: LocalTime,
}

impl DdnsSyncHistory {
    pub fn new(client_uuid: impl Into<String>, status: impl Into<String>) -> Self {
        Self {
            id: 0,
            client_uuid: client_uuid.into(),
            status: status.into(),
            synced_at: LocalTime::now(),
            ..Self::default()
        }
    }
}

impl FromRow for DdnsSyncHistory {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            client_uuid: row.get("client_uuid")?,
            client_name: row.get("client_name")?,
            hostname: row.get("hostname")?,
            record_type: row.get("record_type")?,
            ipv4: row.get("ip_v4")?,
            ipv6: row.get("ip_v6")?,
            record_id: row.get("record_id")?,
            status: row.get("status")?,
            error: row.get("error")?,
            triggered_by: row.get("triggered_by")?,
            synced_at: row.get("synced_at")?,
        })
    }
}
