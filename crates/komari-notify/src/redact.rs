//! Redaction of sensitive provider configuration for outbound API responses.
//!
//! Database rows keep the complete `addition` JSON so message delivery still
//! works. Call [`redact_addition`] only when returning provider configuration
//! to an external caller; never write its result back to the database.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::registry::registry;

/// Remove every sensitive field from a provider `addition` JSON blob.
///
/// The original string is not modified. Invalid JSON, a top-level `null`, or
/// any non-object value yields an empty object, so callers never receive the
/// original secret-bearing text.
///
/// ```
/// let safe = komari_notify::redact_addition(
///     "telegram",
///     r#"{"bot_token":"secret","chat_id":"42"}"#,
/// );
/// assert!(safe.get("bot_token").is_none());
/// assert_eq!(safe["chat_id"], "42");
/// ```
pub fn redact_addition(provider_name: &str, addition: &str) -> Value {
    let Ok(value) = serde_json::from_str::<Value>(addition) else {
        return empty_object();
    };
    redact_value(provider_name, value)
}

/// Redact a serializable configuration value for `Debug` output.
pub(crate) fn redact_serialized<T: serde::Serialize>(provider_name: &str, value: &T) -> Value {
    let value = serde_json::to_value(value).unwrap_or(Value::Null);
    redact_value(provider_name, value)
}

pub(crate) fn redact_value(provider_name: &str, value: Value) -> Value {
    let Some(fields) = registry().sensitive_fields(provider_name) else {
        // Unknown provider: returning the input could leak arbitrary secrets.
        return empty_object();
    };

    let Value::Object(mut map) = value else {
        return empty_object();
    };

    let sensitive = fields
        .iter()
        .map(|field| normalize_key(field))
        .collect::<HashSet<_>>();
    map.retain(|key, _| !sensitive.contains(&normalize_key(key)));

    if provider_name == "webhook" {
        remove_credentialed_webhook_url(&mut map);
    }

    Value::Object(map)
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

/// Compare keys ignoring case, underscores and dashes, so `bot_token`,
/// `BotToken` and `bot-token` are treated the same way.
fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn remove_credentialed_webhook_url(map: &mut Map<String, Value>) {
    let Some(Value::String(raw)) = map.get("url") else {
        return;
    };
    if url_has_credentials(raw) {
        map.remove("url");
    }
}

/// True when a URL carries userinfo or a query parameter that looks like a
/// credential. Unparseable URLs containing obvious credential markers are
/// treated as sensitive too.
pub(crate) fn url_has_credentials(raw: &str) -> bool {
    if let Ok(url) = reqwest::Url::parse(raw) {
        if !url.username().is_empty() || url.password().is_some() {
            return true;
        }
        return url.query_pairs().any(|(key, _)| {
            let key = normalize_key(&key);
            key.contains("token")
                || key.contains("secret")
                || key.contains("password")
                || key.contains("auth")
                || key == "key"
                || key.ends_with("key")
        });
    }

    let lower = raw.to_ascii_lowercase();
    lower.contains('@')
        || lower.contains("token=")
        || lower.contains("secret=")
        || lower.contains("password=")
        || lower.contains("key=")
}

/// URL rendering used by `Debug`: never show userinfo or credential-bearing
/// query parameters.
pub(crate) fn redact_url_for_debug(raw: &str) -> String {
    if !url_has_credentials(raw) {
        return raw.to_owned();
    }

    let Ok(mut url) = reqwest::Url::parse(raw) else {
        return "<redacted>".to_owned();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let retained = url
        .query_pairs()
        .filter(|(key, _)| {
            let key = normalize_key(key);
            !(key.contains("token")
                || key.contains("secret")
                || key.contains("password")
                || key.contains("auth")
                || key == "key"
                || key.ends_with("key"))
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    if !retained.is_empty() {
        url.query_pairs_mut().extend_pairs(retained);
    }
    url.to_string()
}

/// Header names whose values must not appear in `Debug` output.
pub(crate) fn is_sensitive_header(name: &str) -> bool {
    let name = normalize_key(name);
    name == "authorization"
        || name == "proxyauthorization"
        || name == "cookie"
        || name == "setcookie"
        || name.contains("token")
        || name.contains("secret")
        || name.contains("apikey")
        || name.ends_with("key")
}
