use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::FromRow;
use crate::time::LocalTime;
use crate::util::generate_token;

/// A registered client device (`clients` table).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct Client {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub uuid: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub token: String,
    pub name: String,
    pub cpu_name: String,
    pub virtualization: String,
    pub arch: String,
    pub cpu_cores: i64,
    pub os: String,
    pub kernel_version: String,
    pub gpu_name: String,
    #[serde(rename = "ipv4", skip_serializing_if = "String::is_empty")]
    pub ipv4: String,
    #[serde(rename = "ipv6", skip_serializing_if = "String::is_empty")]
    pub ipv6: String,
    pub region: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub remark: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub public_remark: String,
    pub mem_total: i64,
    pub swap_total: i64,
    pub disk_total: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub version: String,
    pub weight: i64,
    pub price: f64,
    pub billing_cycle: i64,
    pub auto_renewal: bool,
    pub currency: String,
    pub expired_at: LocalTime,
    pub group: String,
    /// Semicolon-separated tags, matching the Go implementation.
    pub tags: String,
    pub ddns_enabled: bool,
    pub ddns_hostname: String,
    pub ddns_record_id: String,
    pub ddns_record_type: String,
    pub hidden: bool,
    pub traffic_limit: i64,
    pub traffic_limit_type: String,
    pub created_at: LocalTime,
    pub updated_at: LocalTime,
}

impl Client {
    /// Create a client with the same defaults as `clients.CreateClient`.
    pub fn new(name: impl Into<String>) -> Self {
        let uuid = Uuid::new_v4().to_string();
        let name = {
            let name = name.into();
            if name.is_empty() {
                format!("client_{}", &uuid[0..8])
            } else {
                name
            }
        };
        let now = LocalTime::now();
        Self {
            uuid,
            token: generate_token(),
            name,
            currency: "$".to_owned(),
            ddns_record_type: "inherit".to_owned(),
            traffic_limit_type: "max".to_owned(),
            created_at: now,
            updated_at: now,
            ..Self::default()
        }
    }

    /// Whether the client's paid period has ended.
    pub fn is_expired(&self) -> bool {
        if self.expired_at.is_zero() {
            return false;
        }
        !self.expired_at.is_zero() && self.expired_at < LocalTime::now()
    }
}

impl FromRow for Client {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            uuid: row.get("uuid")?,
            token: row.get("token")?,
            name: row.get("name")?,
            cpu_name: row.get("cpu_name")?,
            virtualization: row.get("virtualization")?,
            arch: row.get("arch")?,
            cpu_cores: row.get("cpu_cores")?,
            os: row.get("os")?,
            kernel_version: row.get("kernel_version")?,
            gpu_name: row.get("gpu_name")?,
            ipv4: row.get("ipv4")?,
            ipv6: row.get("ipv6")?,
            region: row.get("region")?,
            remark: row.get("remark")?,
            public_remark: row.get("public_remark")?,
            mem_total: row.get("mem_total")?,
            swap_total: row.get("swap_total")?,
            disk_total: row.get("disk_total")?,
            version: row.get("version")?,
            weight: row.get("weight")?,
            price: row.get("price")?,
            billing_cycle: row.get("billing_cycle")?,
            auto_renewal: row.get("auto_renewal")?,
            currency: row.get("currency")?,
            expired_at: row.get("expired_at")?,
            group: row.get("group")?,
            tags: row.get("tags")?,
            ddns_enabled: row.get("ddns_enabled")?,
            ddns_hostname: row.get("ddns_hostname")?,
            ddns_record_id: row.get("ddns_record_id")?,
            ddns_record_type: row.get("ddns_record_type")?,
            hidden: row.get("hidden")?,
            traffic_limit: row.get("traffic_limit")?,
            traffic_limit_type: row.get("traffic_limit_type")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}
