use serde::{Deserialize, Serialize};

use super::FromRow;
use crate::time::LocalTime;

/// A single key/value entry in the `configs` table (Komari >= 1.1.4).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigItem {
    pub key: String,
    /// JSON-encoded value, exactly as Go's `ConfigItem.Value` stores it.
    pub value: String,
}

impl FromRow for ConfigItem {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            key: row.get("key")?,
            value: row.get("value")?,
        })
    }
}

/// Legacy single-row `configs` model (`komari/database/models/config.go`).
///
/// It is kept for source compatibility with the Go model. New code should use
/// the key/value [`ConfigItem`] table, which is what current Komari versions
/// (including the bundled `data/komari.db`) use.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct Config {
    #[serde(skip_serializing_if = "crate::models::is_zero_i64")]
    pub id: i64,
    pub sitename: String,
    pub description: String,
    pub allow_cors: bool,
    pub theme: String,
    pub private_site: bool,
    pub api_key: String,
    pub auto_discovery_key: String,
    pub script_domain: String,
    pub send_ip_addr_to_guest: bool,
    pub eula_accepted: bool,
    pub geo_ip_enabled: bool,
    pub geo_ip_provider: String,
    pub ddns_enabled: bool,
    pub ddns_provider: String,
    pub ddns_sync_interval: i64,
    #[serde(rename = "o_auth_enabled")]
    pub oauth_enabled: bool,
    #[serde(rename = "o_auth_provider")]
    pub oauth_provider: String,
    pub disable_password_login: bool,
    pub custom_head: String,
    pub custom_body: String,
    pub notification_enabled: bool,
    pub notification_method: String,
    pub notification_template: String,
    pub expire_notification_enabled: bool,
    pub expire_notification_lead_days: i64,
    pub login_notification: bool,
    pub traffic_limit_percentage: f64,
    pub record_enabled: bool,
    pub record_preserve_time: i64,
    #[serde(rename = "CreatedAt")]
    pub created_at: LocalTime,
    #[serde(rename = "UpdatedAt")]
    pub updated_at: LocalTime,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            id: 0,
            sitename: String::new(),
            description: String::new(),
            allow_cors: false,
            theme: "default".to_owned(),
            private_site: false,
            api_key: String::new(),
            auto_discovery_key: String::new(),
            script_domain: String::new(),
            send_ip_addr_to_guest: false,
            eula_accepted: false,
            geo_ip_enabled: true,
            geo_ip_provider: "ip-api".to_owned(),
            ddns_enabled: false,
            ddns_provider: "none".to_owned(),
            ddns_sync_interval: 10,
            oauth_enabled: false,
            oauth_provider: "github".to_owned(),
            disable_password_login: false,
            custom_head: String::new(),
            custom_body: String::new(),
            notification_enabled: false,
            notification_method: "none".to_owned(),
            notification_template: "{{emoji}}{{emoji}}{{emoji}}\nEvent: {{event}}\nClients: {{client}}\nMessage: {{message}}\nTime: {{time}}".to_owned(),
            expire_notification_enabled: false,
            expire_notification_lead_days: 7,
            login_notification: false,
            traffic_limit_percentage: 80.0,
            record_enabled: true,
            record_preserve_time: 720,
            created_at: LocalTime::default(),
            updated_at: LocalTime::default(),
        }
    }
}
