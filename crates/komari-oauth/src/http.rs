//! Small HTTP/JSON helpers shared by the providers.

use reqwest::Response;
use serde_json::Value;

use crate::error::{OAuthError, Result, redact_secrets};
use crate::types::TokenSet;

/// Read a response body as JSON, turning non-2xx responses and parse errors
/// into redacted [`OAuthError`]s.
pub async fn json_response(response: Response, secrets: &[&str]) -> Result<Value> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| OAuthError::Http(redact_secrets(&error.to_string(), secrets)))?;
    if !status.is_success() {
        return Err(OAuthError::Http(format!(
            "HTTP {status}: {}",
            redact_secrets(&body, secrets)
        )));
    }
    serde_json::from_str(&body).map_err(|error| {
        OAuthError::InvalidResponse(format!("{error}: {}", redact_secrets(&body, secrets)))
    })
}

/// Parse a token endpoint response.
///
/// `expires_in` is accepted as either a JSON number or a numeric string, which
/// is what several providers do in practice.
pub fn parse_token_set(value: &Value, secrets: &[&str]) -> Result<TokenSet> {
    if let Some(error) = value.get("error") {
        let code = error.as_str().unwrap_or("unknown_error").to_owned();
        let description = value
            .get("error_description")
            .and_then(Value::as_str)
            .map(|description| redact_secrets(description, secrets))
            .unwrap_or_else(|| "no description".to_owned());
        return Err(OAuthError::TokenExchange(format!("{code}: {description}")));
    }

    let access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            OAuthError::TokenExchange("response did not contain an access_token".to_owned())
        })?
        .to_owned();

    let expires_in = value.get("expires_in").and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
    });

    Ok(TokenSet {
        access_token,
        token_type: value
            .get("token_type")
            .and_then(Value::as_str)
            .unwrap_or("Bearer")
            .to_owned(),
        refresh_token: string_field(value, "refresh_token"),
        expires_in,
        expires_at: None,
        scope: string_field(value, "scope"),
        id_token: string_field(value, "id_token"),
        raw: Some(value.clone()),
        id_token_claims: None,
    }
    .with_expiry_from_now())
}

/// Extract a non-empty string field.
pub fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Convert a JSON scalar to the same textual form as Go's `fmt.Sprintf("%v", v)`.
pub fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Build an OAuth error response from callback query parameters.
pub fn callback_error(query: &crate::types::CallbackQuery) -> Option<OAuthError> {
    let code = query.get("error").filter(|value| !value.is_empty())?;
    let description = query
        .get("error_description")
        .cloned()
        .unwrap_or_else(|| "no description".to_owned());
    Some(OAuthError::provider_error(
        code.clone(),
        description,
        query.get("error_uri").cloned(),
    ))
}
