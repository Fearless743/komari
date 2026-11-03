//! `GET /api/public` — public site settings.
//!
//! Port of Go `api/public.GetPublicSettings` and `database.GetPublicInfo`.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use komari_config::keys;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::state::ApiState;

use super::{cookie_value, respond_error, respond_success};

const DEFAULT_SITENAME: &str = "Komari";
const DEFAULT_DESCRIPTION: &str = "Komari Monitor, a simple server monitoring tool.";
const DEFAULT_THEME: &str = "default";
const DEFAULT_OAUTH_PROVIDER: &str = "github";
const DEFAULT_RECORD_PRESERVE_TIME: i64 = 720;

const TEMP_SHARE_TOKEN_KEY: &str = "tempory_share_token";
const TEMP_SHARE_EXPIRE_KEY: &str = "tempory_share_token_expire_at";
const TEMP_ACCESS_COOKIE: &str = "temp_key";

pub(crate) async fn get_public_settings(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Response {
    match public_info(&state, &headers) {
        Ok(info) => respond_success(info),
        Err(error) => respond_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

fn public_info(state: &ApiState, headers: &HeaderMap) -> Result<Value, String> {
    let config = state.db.config();
    let read_string = |key: &str| -> Result<Option<String>, String> {
        config
            .get_as::<String>(key)
            .map_err(|error| error.to_string())
    };
    let read_bool = |key: &str| -> Result<Option<bool>, String> {
        config
            .get_as::<bool>(key)
            .map_err(|error| error.to_string())
    };
    let read_i64 = |key: &str| -> Result<Option<i64>, String> {
        config.get_as::<i64>(key).map_err(|error| error.to_string())
    };

    let sitename = read_string(keys::SITENAME_KEY)?.unwrap_or_else(|| DEFAULT_SITENAME.to_owned());
    let description =
        read_string(keys::DESCRIPTION_KEY)?.unwrap_or_else(|| DEFAULT_DESCRIPTION.to_owned());
    let custom_head = read_string(keys::CUSTOM_HEAD_KEY)?.unwrap_or_default();
    let custom_body = read_string(keys::CUSTOM_BODY_KEY)?.unwrap_or_default();
    let oauth_enable = read_bool(keys::OAUTH_ENABLED_KEY)?.unwrap_or(false);
    let oauth_provider =
        read_string(keys::OAUTH_PROVIDER_KEY)?.unwrap_or_else(|| DEFAULT_OAUTH_PROVIDER.to_owned());
    let disable_password_login = read_bool(keys::DISABLE_PASSWORD_LOGIN_KEY)?.unwrap_or(false);
    let allow_cors = read_bool(keys::ALLOW_CORS_KEY)?.unwrap_or(false);
    let record_enabled = read_bool(keys::RECORD_ENABLED_KEY)?.unwrap_or(true);
    let record_preserve_time =
        read_i64(keys::RECORD_PRESERVE_TIME_KEY)?.unwrap_or(DEFAULT_RECORD_PRESERVE_TIME);
    let mut private_site = read_bool(keys::PRIVATE_SITE_KEY)?.unwrap_or(false);
    let theme = read_string(keys::THEME_KEY)?.unwrap_or_else(|| DEFAULT_THEME.to_owned());

    if has_temp_access(state, headers) {
        private_site = false;
    }

    let mut theme_settings = match state.db.themes().get_data(&theme) {
        Ok(Value::Object(map)) => map,
        // Go unmarshals into `gin.H{}` and leaves it empty on invalid JSON.
        Ok(_) | Err(_) => Map::new(),
    };
    merge_theme_defaults(&theme, &mut theme_settings);

    Ok(json!({
        "sitename": sitename,
        "description": description,
        "custom_head": custom_head,
        "custom_body": custom_body,
        "oauth_enable": oauth_enable,
        "oauth_provider": oauth_provider,
        "disable_password_login": disable_password_login,
        "allow_cors": allow_cors,
        "record_enabled": record_enabled,
        "record_preserve_time": record_preserve_time,
        "private_site": private_site,
        "theme": theme,
        "theme_settings": Value::Object(theme_settings),
    }))
}

/// Port of Go `hasTempAccess` (also used by the private-site middleware).
fn has_temp_access(state: &ApiState, headers: &HeaderMap) -> bool {
    let Some(cookie) = cookie_value(headers, TEMP_ACCESS_COOKIE) else {
        return false;
    };
    if cookie.is_empty() {
        return false;
    }
    let Ok(Some(allow_key)) = state.db.config().get_as::<String>(TEMP_SHARE_TOKEN_KEY) else {
        return false;
    };
    let Ok(Some(expire_at)) = state.db.config().get_as::<i64>(TEMP_SHARE_EXPIRE_KEY) else {
        return false;
    };
    !allow_key.is_empty() && cookie == allow_key && expire_at >= now_unix()
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

/// Merge defaults declared in `./data/theme/<short>/komari-theme.json`.
///
/// Go only does this for managed themes (anything except `default`).
fn merge_theme_defaults(theme: &str, settings: &mut Map<String, Value>) {
    if theme.is_empty() || theme == DEFAULT_THEME {
        return;
    }
    let path = PathBuf::from("./data/theme")
        .join(theme)
        .join("komari-theme.json");
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(declaration) = serde_json::from_str::<ThemeDeclaration>(&contents) else {
        return;
    };
    if declaration.configuration.kind != "managed" {
        return;
    }
    for item in declaration.configuration.data {
        if item.key.is_empty() || settings.contains_key(&item.key) {
            continue;
        }
        let mut default = item.default;
        if item.item_type == "select"
            && (default.is_null() || default.as_str().is_some_and(str::is_empty))
        {
            if let Some(first) = item
                .options
                .split(',')
                .map(str::trim)
                .find(|option| !option.is_empty())
            {
                default = Value::String(first.to_owned());
            }
        }
        if default.is_null() {
            default = match item.item_type.as_str() {
                "number" => json!(0),
                "switch" => json!(false),
                _ => json!(""),
            };
        }
        settings.insert(item.key, default);
    }
}

#[derive(Deserialize, Default)]
struct ThemeDeclaration {
    #[serde(default)]
    configuration: ThemeConfigurationDecl,
}

#[derive(Deserialize, Default)]
struct ThemeConfigurationDecl {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    data: Vec<ManagedItem>,
}

#[derive(Deserialize, Default)]
struct ManagedItem {
    #[serde(default)]
    key: String,
    #[serde(rename = "type", default)]
    item_type: String,
    #[serde(default)]
    options: String,
    #[serde(default)]
    default: Value,
}
