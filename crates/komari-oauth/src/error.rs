//! Error types shared by the OAuth providers and the account linker.

use thiserror::Error;

/// Errors returned by [`crate::OAuthProvider`] implementations and the
/// account-linking helpers.
#[derive(Debug, Error)]
pub enum OAuthError {
    /// The requested provider name is not registered in the factory.
    #[error("oauth provider {0} not found")]
    ProviderNotFound(String),

    /// The provider configuration JSON is missing a required field or is
    /// otherwise unusable.
    #[error("invalid provider configuration: {0}")]
    InvalidConfig(String),

    /// The `state` parameter is missing, unknown, expired or otherwise does
    /// not match the value created by [`crate::OAuthProvider::authorization_url`].
    #[error("invalid state")]
    InvalidState,

    /// The authorization code is missing from the callback query.
    #[error("no authorization code provided")]
    MissingCode,

    /// The redirect URI is not an absolute `http`/`https` URL without a
    /// fragment.
    #[error("invalid redirect URI: {0}")]
    InvalidRedirectUri(String),

    /// The redirect URI used for the callback differs from the one used when
    /// the authorization URL was created.
    #[error("redirect URI does not match the authorization request")]
    RedirectUriMismatch,

    /// The provider redirected back with an OAuth error response
    /// (`?error=access_denied&error_description=...`).
    #[error("provider returned error {code}: {description}")]
    ProviderError {
        code: String,
        description: String,
        uri: Option<String>,
    },

    /// The token endpoint returned an error or an unusable response.
    #[error("token exchange failed: {0}")]
    TokenExchange(String),

    /// The user-info endpoint returned an error or an unusable response.
    #[error("user info request failed: {0}")]
    UserInfo(String),

    /// An `id_token` or Cloudflare Access JWT failed validation.
    #[error("invalid token: {0}")]
    InvalidToken(String),

    /// A JWKS document could not be fetched or parsed.
    #[error("JWKS error: {0}")]
    Jwks(String),

    /// An HTTP transport error. The message has already had secrets removed.
    #[error("http error: {0}")]
    Http(String),

    /// A response body was not valid JSON or had an unexpected shape. The
    /// body has already had secrets removed.
    #[error("invalid response: {0}")]
    InvalidResponse(String),

    /// JSON (de)serialization failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Database access failed.
    #[error("database error: {0}")]
    Database(#[from] komari_db::DbError),

    /// No local account is bound to this external identity. The API layer
    /// should return 401 and ask the user to bind first, matching Go.
    #[error("please log in and bind your external account first.")]
    AccountNotBound,

    /// The external identity is already bound to a different local account.
    #[error("external account is already bound to another user")]
    AlreadyBound,

    /// The local account already has a different external account bound.
    #[error("user already has an external account bound")]
    UserAlreadyBound,

    /// An external identity had an empty provider name or subject.
    #[error("invalid external identity: {0}")]
    InvalidIdentity(String),

    /// The provider does not implement the requested optional operation.
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}

/// Convenience result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, OAuthError>;

impl OAuthError {
    /// Build a [`OAuthError::ProviderError`] with already-redacted values.
    pub fn provider_error(
        code: impl Into<String>,
        description: impl Into<String>,
        uri: Option<String>,
    ) -> Self {
        Self::ProviderError {
            code: code.into(),
            description: description.into(),
            uri,
        }
    }
}

/// Render an error for logs while guaranteeing that the given secrets never
/// appear in the output.
pub fn redacted_error(error: &OAuthError, secrets: &[&str]) -> String {
    redact_secrets(&error.to_string(), secrets)
}

/// Replace every occurrence of the supplied secrets (and common OAuth query
/// parameters) with `********`.
///
/// Providers call this before turning a transport or response error into an
/// [`OAuthError`]. `client_secret`, `app_key`, `access_token`, `refresh_token`
/// and `id_token` values are also masked when they appear as query/form
/// parameters, so an error URL can be logged safely.
pub fn redact_secrets(message: &str, secrets: &[&str]) -> String {
    let mut result = message.to_owned();

    let mut secrets: Vec<&str> = secrets
        .iter()
        .copied()
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
        .collect();
    // Replace longer values first so a secret that contains another secret is
    // masked completely.
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    secrets.dedup();
    for secret in secrets {
        result = result.replace(secret, "********");
    }

    for parameter in [
        "client_secret",
        "appkey",
        "access_token",
        "refresh_token",
        "id_token",
    ] {
        result = redact_query_parameter(&result, parameter);
    }
    redact_json_document(&result)
}

fn redact_json_document(input: &str) -> String {
    let Some(start) = input.find(['{', '[']) else {
        return input.to_owned();
    };
    let Some(end) = input.rfind(['}', ']']) else {
        return input.to_owned();
    };
    if start >= end {
        return input.to_owned();
    }
    let document = &input[start..=end];
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(document) else {
        return input.to_owned();
    };
    redact_json_value(&mut value);
    let Ok(redacted) = serde_json::to_string(&value) else {
        return input.to_owned();
    };
    format!("{}{}{}", &input[..start], redacted, &input[end + 1..])
}

fn redact_json_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(values) => {
            for (key, value) in values.iter_mut() {
                if is_sensitive_json_key(key) {
                    *value = serde_json::Value::String("********".to_owned());
                } else {
                    redact_json_value(value);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_json_value(value);
            }
        }
        _ => {}
    }
}

fn is_sensitive_json_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "access_token"
            | "refresh_token"
            | "id_token"
            | "client_secret"
            | "app_key"
            | "appkey"
            | "secret"
            | "code"
    )
}

fn redact_query_parameter(input: &str, parameter: &str) -> String {
    let needle = format!("{parameter}=");
    let mut output = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(index) = rest.find(&needle) {
        let (before, after) = rest.split_at(index);
        output.push_str(before);
        output.push_str(&needle);
        output.push_str("********");

        let value = &after[needle.len()..];
        let end = value
            .find(|character: char| {
                matches!(
                    character,
                    '&' | '#' | ' ' | '\t' | '\n' | '\r' | '"' | '\'' | '}' | ']'
                )
            })
            .unwrap_or(value.len());
        rest = &value[end..];
    }
    output.push_str(rest);
    output
}

/// A helper for implementing [`fmt::Debug`] on provider configurations without
/// leaking secrets.
pub(crate) fn debug_secret(value: &str) -> &'static str {
    if value.is_empty() { "" } else { "********" }
}

#[cfg(test)]
mod tests {
    use super::redact_secrets;

    #[test]
    fn redacts_explicit_secrets_and_query_parameters() {
        let message = "POST https://idp/token?client_secret=s3cr3t&appkey=qq-key \
                       failed for s3cr3t with access_token=tok-123 and id_token=jwt-456";
        let redacted = redact_secrets(message, &["s3cr3t"]);
        assert!(!redacted.contains("s3cr3t"));
        assert!(!redacted.contains("qq-key"));
        assert!(!redacted.contains("tok-123"));
        assert!(!redacted.contains("jwt-456"));
        assert!(redacted.contains("********"));
    }

    #[test]
    fn redacts_json_token_and_secret_values() {
        let message = r#"HTTP 400: {"error":"invalid_grant","access_token":"tok-123","refresh_token":"ref-456","client_secret":"s3cr3t","nested":{"id_token":"jwt-789"}}"#;
        let redacted = redact_secrets(message, &[]);
        assert!(redacted.contains("invalid_grant"));
        assert!(!redacted.contains("tok-123"));
        assert!(!redacted.contains("ref-456"));
        assert!(!redacted.contains("s3cr3t"));
        assert!(!redacted.contains("jwt-789"));
        assert!(redacted.contains("********"));
    }
}
