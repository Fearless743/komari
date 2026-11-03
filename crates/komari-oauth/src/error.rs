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
/// [`OAuthError`]. Parameter names are normalized before comparison, so
/// `access_token`, `accessToken` and `access-token` are all masked in query,
/// form and JSON contexts.
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

    result = redact_query_parameters(&result);
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
                if is_sensitive_key(key) {
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

/// Normalize a JSON key by removing separators and lowercasing it.
///
/// This makes `access_token`, `accessToken`, `access-token` and
/// `access.token` equivalent.
pub(crate) fn is_sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| !matches!(character, '_' | '-' | '.' | ' '))
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "clientsecret"
            | "appsecret"
            | "appkey"
            | "secret"
            | "code"
            | "state"
            | "token"
            | "password"
            | "passwd"
            | "twofactor"
            | "twofactorsecret"
            | "totp"
            | "totpsecret"
    )
}

fn redact_query_parameters(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut copied_until = 0;
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'=' {
            let mut key_start = index;
            while key_start > 0 && is_parameter_name_byte(bytes[key_start - 1]) {
                key_start -= 1;
            }
            if key_start < index {
                let key = &input[key_start..index];
                if is_sensitive_key(key) {
                    output.push_str(&input[copied_until..key_start]);
                    output.push_str(key);
                    output.push_str("=********");
                    let mut value_end = index + 1;
                    while value_end < bytes.len()
                        && !matches!(
                            bytes[value_end],
                            b'&' | b'#' | b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b'}' | b']'
                        )
                    {
                        value_end += 1;
                    }
                    copied_until = value_end;
                    index = value_end;
                    continue;
                }
            }
        }
        index += 1;
    }
    output.push_str(&input[copied_until..]);
    output
}

fn is_parameter_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
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
    fn redacts_normalized_query_and_form_parameters() {
        let message = "https://idp/cb?accessToken=camel-value&access-token=kebab-value&clientSecret=client-value&idToken=jwt-value&code=auth-code&state=csrf-state&passwd=hash-value&two_factor=totp-value&totp_secret=totp-key-value";
        let redacted = redact_secrets(message, &[]);
        for value in [
            "camel-value",
            "kebab-value",
            "client-value",
            "jwt-value",
            "auth-code",
            "csrf-state",
            "hash-value",
            "totp-value",
            "totp-key-value",
        ] {
            assert!(!redacted.contains(value), "leaked {value}: {redacted}");
        }
        assert!(redacted.contains("********"));
    }

    #[test]
    fn redacts_json_account_secrets() {
        let message = r#"{"passwd":"hash-value","two_factor":"totp-secret","totp_secret":"totp-key","safe":"ok"}"#;
        let redacted = redact_secrets(message, &[]);
        assert!(!redacted.contains("hash-value"));
        assert!(!redacted.contains("totp-secret"));
        assert!(!redacted.contains("totp-key"));
        assert!(redacted.contains("ok"));
    }

    #[test]
    fn redacts_json_token_and_secret_values() {
        let message = r#"HTTP 400: {"error":"invalid_grant","access_token":"tok-123","refreshToken":"ref-456","client-secret":"s3cr3t","nested":{"id.token":"jwt-789"}}"#;
        let redacted = redact_secrets(message, &[]);
        assert!(redacted.contains("invalid_grant"));
        assert!(!redacted.contains("tok-123"));
        assert!(!redacted.contains("ref-456"));
        assert!(!redacted.contains("s3cr3t"));
        assert!(!redacted.contains("jwt-789"));
        assert!(redacted.contains("********"));
    }
}
