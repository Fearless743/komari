//! Strongly typed view of the site settings persisted in the `configs` table.
//!
//! The field names and defaults mirror `config.Legacy` in the Go backend.
//! Loading and saving these values is the database crate's responsibility; this
//! type exists so every crate shares one definition.

use serde::{Deserialize, Serialize};

pub const DEFAULT_SITENAME: &str = "Komari";
pub const DEFAULT_DESCRIPTION: &str = "A simple server monitor tool.";
pub const DEFAULT_NOTIFICATION_TEMPLATE: &str = "{{emoji}}{{emoji}}{{emoji}}\nEvent: {{event}}\nClients: {{client}}\nMessage: {{message}}\nTime: {{time}}";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteConfig {
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
    pub base_scripts_url: String,
    pub geo_ip_enabled: bool,
    pub geo_ip_provider: String,
    pub ddns_enabled: bool,
    pub ddns_provider: String,
    pub ddns_sync_interval: i32,
    pub o_auth_enabled: bool,
    pub o_auth_provider: String,
    pub disable_password_login: bool,
    pub custom_head: String,
    pub custom_body: String,
    pub notification_enabled: bool,
    pub notification_method: String,
    pub notification_template: String,
    pub expire_notification_enabled: bool,
    pub expire_notification_lead_days: i32,
    pub login_notification: bool,
    pub traffic_limit_percentage: f64,
    pub record_enabled: bool,
    pub record_preserve_time: i32,
}

impl Default for SiteConfig {
    fn default() -> Self {
        Self {
            sitename: DEFAULT_SITENAME.to_owned(),
            description: DEFAULT_DESCRIPTION.to_owned(),
            allow_cors: false,
            theme: "default".to_owned(),
            private_site: false,
            api_key: String::new(),
            auto_discovery_key: String::new(),
            script_domain: String::new(),
            send_ip_addr_to_guest: false,
            eula_accepted: false,
            base_scripts_url: String::new(),
            geo_ip_enabled: true,
            geo_ip_provider: "ipinfo".to_owned(),
            ddns_enabled: false,
            ddns_provider: "none".to_owned(),
            ddns_sync_interval: 10,
            o_auth_enabled: false,
            o_auth_provider: "github".to_owned(),
            disable_password_login: false,
            custom_head: String::new(),
            custom_body: String::new(),
            notification_enabled: true,
            notification_method: "none".to_owned(),
            notification_template: DEFAULT_NOTIFICATION_TEMPLATE.to_owned(),
            expire_notification_enabled: true,
            expire_notification_lead_days: 7,
            login_notification: true,
            traffic_limit_percentage: 80.0,
            record_enabled: true,
            record_preserve_time: 720,
        }
    }
}
