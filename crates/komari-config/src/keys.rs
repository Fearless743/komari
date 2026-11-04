//! Names of environment variables and persisted configuration keys.
//!
//! The persisted keys are the JSON tags used by the Go backend's `configs`
//! table, and the environment variables are the ones read by the Go CLI. Both
//! must stay stable for compatibility.

// ---------------------------------------------------------------------------
// Environment variables
// ---------------------------------------------------------------------------

pub const ENV_LISTEN: &str = "KOMARI_LISTEN";
pub const ENV_DB_TYPE: &str = "KOMARI_DB_TYPE";
pub const ENV_DB_FILE: &str = "KOMARI_DB_FILE";
pub const ENV_DB_HOST: &str = "KOMARI_DB_HOST";
pub const ENV_DB_PORT: &str = "KOMARI_DB_PORT";
pub const ENV_DB_USER: &str = "KOMARI_DB_USER";
pub const ENV_DB_PASS: &str = "KOMARI_DB_PASS";
pub const ENV_DB_NAME: &str = "KOMARI_DB_NAME";
pub const ENV_LOG_LEVEL: &str = "KOMARI_LOG_LEVEL";
pub const ENV_THEME: &str = "KOMARI_THEME";
pub const ENV_ALLOW_CORS: &str = "KOMARI_ALLOW_CORS";
pub const ENV_ENABLE_CLOUDFLARED: &str = "KOMARI_ENABLE_CLOUDFLARED";
pub const ENV_CLOUDFLARED_TOKEN: &str = "KOMARI_CLOUDFLARED_TOKEN";
pub const ENV_WS_DISABLE_ORIGIN: &str = "KOMARI_WS_DISABLE_ORIGIN";
pub const ENV_BODY_LIMIT: &str = "KOMARI_BODY_LIMIT";

// ---------------------------------------------------------------------------
// Persisted keys (`configs` table, JSON representation)
// ---------------------------------------------------------------------------

pub const SITENAME_KEY: &str = "sitename";
pub const DESCRIPTION_KEY: &str = "description";
pub const ALLOW_CORS_KEY: &str = "allow_cors";
pub const THEME_KEY: &str = "theme";
pub const PRIVATE_SITE_KEY: &str = "private_site";
pub const API_KEY_KEY: &str = "api_key";
pub const SCRIPT_DOMAIN_KEY: &str = "script_domain";
pub const SEND_IP_ADDR_TO_GUEST_KEY: &str = "send_ip_addr_to_guest";
pub const EULA_ACCEPTED_KEY: &str = "eula_accepted";
pub const BASE_SCRIPTS_URL_KEY: &str = "base_scripts_url";
pub const GEO_IP_ENABLED_KEY: &str = "geo_ip_enabled";
pub const GEO_IP_PROVIDER_KEY: &str = "geo_ip_provider";
pub const DDNS_ENABLED_KEY: &str = "ddns_enabled";
pub const DDNS_PROVIDER_KEY: &str = "ddns_provider";
pub const DDNS_SYNC_INTERVAL_KEY: &str = "ddns_sync_interval";
pub const OAUTH_ENABLED_KEY: &str = "o_auth_enabled";
pub const OAUTH_PROVIDER_KEY: &str = "o_auth_provider";
pub const DISABLE_PASSWORD_LOGIN_KEY: &str = "disable_password_login";
pub const CUSTOM_HEAD_KEY: &str = "custom_head";
pub const CUSTOM_BODY_KEY: &str = "custom_body";
pub const NOTIFICATION_ENABLED_KEY: &str = "notification_enabled";
pub const NOTIFICATION_METHOD_KEY: &str = "notification_method";
pub const NOTIFICATION_TEMPLATE_KEY: &str = "notification_template";
pub const EXPIRE_NOTIFICATION_ENABLED_KEY: &str = "expire_notification_enabled";
pub const EXPIRE_NOTIFICATION_LEAD_DAYS_KEY: &str = "expire_notification_lead_days";
pub const LOGIN_NOTIFICATION_KEY: &str = "login_notification";
pub const TRAFFIC_LIMIT_PERCENTAGE_KEY: &str = "traffic_limit_percentage";
pub const RECORD_ENABLED_KEY: &str = "record_enabled";
pub const RECORD_PRESERVE_TIME_KEY: &str = "record_preserve_time";
pub const UPDATED_AT_KEY: &str = "updated_at";
