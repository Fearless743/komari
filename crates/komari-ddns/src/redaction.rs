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
/// This is useful for custom registries and tests. Webhook URLs are always
/// removed because their path/query string may contain provider tokens even
/// when the caller's field list omits `url`.
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
        configuration.remove("url");
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
