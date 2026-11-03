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
///
/// In addition to removing declared secret fields, every URL-shaped field is
/// rendered as scheme + host only, so path tokens (Telegram bot tokens,
/// Server酱 sendkeys, Slack/Discord webhook tokens) never reach logs.
pub(crate) fn redact_serialized<T: serde::Serialize>(provider_name: &str, value: &T) -> Value {
    let Some(fields) = registry().sensitive_fields(provider_name) else {
        return empty_object();
    };
    let value = serde_json::to_value(value).unwrap_or(Value::Null);
    let Value::Object(mut map) = value else {
        return empty_object();
    };

    strip_sensitive_keys(&mut map, fields);
    for (key, value) in &mut map {
        if is_url_field(key) {
            if let Value::String(raw) = value {
                *value = Value::String(redact_url_for_debug(raw));
            }
        }
    }
    Value::Object(map)
}

pub(crate) fn redact_value(provider_name: &str, value: Value) -> Value {
    let Some(fields) = registry().sensitive_fields(provider_name) else {
        // Unknown provider: returning the input could leak arbitrary secrets.
        return empty_object();
    };

    let Value::Object(mut map) = value else {
        return empty_object();
    };

    strip_sensitive_keys(&mut map, fields);

    // All URL-shaped fields are checked, including `url`, `URL`,
    // `server_url`, `endpoint` and `api_url` variants. Credential-bearing URLs
    // are removed rather than partially masked.
    let url_keys = map
        .keys()
        .filter(|key| is_url_field(key))
        .cloned()
        .collect::<Vec<_>>();
    for key in url_keys {
        if let Some(Value::String(raw)) = map.get(&key) {
            if url_has_credentials(raw) {
                map.remove(&key);
            }
        }
    }

    Value::Object(map)
}

fn strip_sensitive_keys(map: &mut Map<String, Value>, fields: &[&str]) {
    let sensitive = fields
        .iter()
        .map(|field| normalize_key(field))
        .collect::<HashSet<_>>();
    map.retain(|key, _| !sensitive.contains(&normalize_key(key)));
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

fn is_url_field(key: &str) -> bool {
    matches!(
        normalize_key(key).as_str(),
        "url" | "serverurl" | "endpoint" | "apiurl"
    )
}

/// Query/header names that carry credentials. The list is intentionally exact
/// (plus a few suffix checks) so unrelated names such as `author` or `monkey`
/// are not redacted.
pub(crate) fn is_sensitive_credential_key(key: &str) -> bool {
    // Exact normalized names only. Substring matching would misclassify
    // `secretary`, `tokenizer` and `signature_pad`.
    matches!(
        normalize_key(key).as_str(),
        "token"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "bearertoken"
            | "authtoken"
            | "apitoken"
            | "csrftoken"
            | "xsrftoken"
            | "securitytoken"
            | "xauthtoken"
            | "xapitoken"
            | "xamzsecuritytoken"
            | "xamzcredential"
            | "awsaccesskeyid"
            | "xgoogcredential"
            | "xauthorization"
            | "xforwardedauthorization"
            | "auth"
            | "authorization"
            | "apikey"
            | "xapikey"
            | "key"
            | "accesskey"
            | "privatekey"
            | "signingkey"
            | "sharedkey"
            | "secret"
            | "clientsecret"
            | "sharedsecret"
            | "appsecret"
            | "consumersecret"
            | "oauthsecret"
            | "secretkey"
            | "password"
            | "passwd"
            | "sig"
            | "signature"
            | "xsignature"
            | "xamzsignature"
            | "xhubsignature"
            | "xhubsignature256"
            | "xgoogsignature"
            | "hmac"
            | "xhmac"
            | "hmacsha256"
            | "digest"
            | "credential"
            | "credentials"
    )
}

/// True when a URL carries userinfo, a credential-like query parameter, or a
/// token embedded in a known webhook path (Slack, Discord, Microsoft Teams).
pub(crate) fn url_has_credentials(raw: &str) -> bool {
    if let Ok(url) = reqwest::Url::parse(raw) {
        if !url.username().is_empty() || url.password().is_some() {
            return true;
        }
        if url_path_has_token(&url) {
            return true;
        }
        return url
            .query_pairs()
            .any(|(key, _)| is_sensitive_credential_key(&key));
    }

    let lower = raw.to_ascii_lowercase();
    lower.contains('@')
        || lower.contains("token=")
        || lower.contains("secret=")
        || lower.contains("password=")
        || lower.contains("signature=")
        || lower.contains("sig=")
        || lower.contains("key=")
}

fn url_path_has_token(url: &reqwest::Url) -> bool {
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let path = url.path();
    let lower_path = path.to_ascii_lowercase();

    if host == "api.telegram.org" && lower_path.starts_with("/bot") {
        // The default endpoint is `/bot`; a token may be appended directly
        // (`/bot123:SECRET`) or after one slash (`/bot/SECRET`).
        let remainder = &lower_path[4..];
        let remainder = remainder.strip_prefix('/').unwrap_or(remainder);
        if !remainder.is_empty() {
            return true;
        }
    }
    if host == "hooks.slack.com" && lower_path.starts_with("/services/") {
        return true;
    }
    if (host == "discord.com"
        || host == "discordapp.com"
        || host.ends_with(".discord.com")
        || host.ends_with(".discordapp.com"))
        && lower_path.starts_with("/api/webhooks/")
    {
        return true;
    }
    if (host.ends_with("office.com") || host.ends_with("office365.com"))
        && (lower_path.contains("/webhook") || lower_path.contains("/incomingwebhook"))
    {
        return true;
    }

    // Common self-hosted patterns: /token/<secret>, /key/<secret>,
    // /secret/<secret>, /webhook/<secret>, /send/<sendkey>. Only a token-shaped
    // segment triggers redaction so `/send/message` and `/webhook/events` are
    // preserved.
    const MARKERS: &[&str] = &["token", "key", "secret", "webhook", "send"];
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    segments.windows(2).any(|pair| {
        let marker = pair[0].to_ascii_lowercase();
        MARKERS.contains(&marker.as_str()) && looks_like_path_token(pair[1])
    })
}

/// Heuristic token shape for self-hosted path segments.
///
/// Accepts uppercase/mixed-case segments, segments containing digits, long
/// separated values, or very long opaque strings. Plain lowercase words such as
/// `message`, `events` and `refresh` stay readable.
fn looks_like_path_token(segment: &str) -> bool {
    if segment.len() < 6 {
        return false;
    }
    let has_upper = segment
        .chars()
        .any(|character| character.is_ascii_uppercase());
    let has_digit = segment.chars().any(|character| character.is_ascii_digit());
    let has_separator = segment.contains('_') || segment.contains('-');
    has_upper || has_digit || (has_separator && segment.len() >= 8) || segment.len() >= 32
}

/// URL rendering used by `Debug`: scheme + host + port only. Paths and query
/// strings are never shown, covering Telegram bot tokens, Server酱 sendkeys and
/// Slack/Discord/Teams path tokens.
pub(crate) fn redact_url_for_debug(raw: &str) -> String {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return "<redacted>".to_owned();
    };
    let host = url.host_str().unwrap_or("<unknown>");
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    if url.path().is_empty() || url.path() == "/" {
        format!("{}://{host}{port}/", url.scheme())
    } else {
        format!("{}://{host}{port}/<path redacted>", url.scheme())
    }
}

/// Header names whose values must not appear in `Debug` output.
pub(crate) fn is_sensitive_header(name: &str) -> bool {
    let name = normalize_key(name);
    matches!(
        name.as_str(),
        "authorization" | "proxyauthorization" | "cookie" | "setcookie" | "xauth"
    ) || is_sensitive_credential_key(&name)
}
