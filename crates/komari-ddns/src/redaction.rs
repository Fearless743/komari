//! Redaction of provider configurations for public API responses.
//!
//! Database rows (`ddns_providers.addition`) are stored and consumed exactly
//! as configured. Only when a configuration is returned to an external caller
//! should it be passed through [`redact_addition`] or
//! [`redact_provider_configuration`], which remove sensitive fields instead of
//! masking them.

use serde_json::{Map, Value};

use crate::factory::factory;
use crate::provider::DdnsProvider;

/// Remove a provider's sensitive fields from a stored `addition` JSON string.
///
/// - Sensitive fields are deleted, not masked.
/// - All other fields are preserved verbatim.
/// - Invalid JSON, top-level `null`, arrays and other non-object values yield
///   an empty object so the original text is never echoed back.
/// - Unknown providers also yield an empty object because their sensitive
///   fields cannot be classified.
///
/// The input string is never modified; the database path can keep using the
/// original `addition`.
pub fn redact_addition(provider_name: &str, addition: &str) -> Value {
    let Some(sensitive_fields) = factory().sensitive_fields(provider_name) else {
        return Value::Object(Map::new());
    };
    redact_addition_with_fields(provider_name, addition, &sensitive_fields)
}

/// Redact a configuration using an explicit sensitive-field list.
///
/// This is useful for custom registries and tests. `webhook` URLs that embed
/// credentials are removed in addition to the supplied fields.
pub fn redact_addition_with_fields(
    provider_name: &str,
    addition: &str,
    sensitive_fields: &[&str],
) -> Value {
    let Ok(Value::Object(mut configuration)) = serde_json::from_str::<Value>(addition) else {
        return Value::Object(Map::new());
    };
    for field in sensitive_fields {
        configuration.remove(*field);
    }
    if provider_name == "webhook" {
        redact_webhook_url(&mut configuration);
    }
    Value::Object(configuration)
}

/// Redact a live provider instance's configuration.
pub fn redact_provider_configuration(provider: &dyn DdnsProvider) -> Value {
    let addition = match provider.configuration() {
        Value::Null => return Value::Object(Map::new()),
        configuration => configuration.to_string(),
    };
    redact_addition_with_fields(provider.name(), &addition, provider.sensitive_fields())
}

fn redact_webhook_url(configuration: &mut Map<String, Value>) {
    let Some(Value::String(url)) = configuration.get("url") else {
        return;
    };
    if url_contains_credentials(url) {
        configuration.remove("url");
    }
}

/// Whether a URL embeds a username or password. Unparseable values are treated
/// as potentially sensitive so they are removed rather than leaked.
pub(crate) fn url_contains_credentials(url: &str) -> bool {
    if url.trim().is_empty() {
        return false;
    }
    match reqwest::Url::parse(url) {
        Ok(parsed) => !parsed.username().is_empty() || parsed.password().is_some(),
        Err(_) => true,
    }
}
