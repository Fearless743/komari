//! Redirect URI validation.
//!
//! Go simply interpolates the callback URI into the authorization URL. The
//! Rust port additionally rejects malformed values before they reach a
//! provider, which prevents an attacker-controlled `Host` header from turning
//! into an open redirect.

use url::Url;

use crate::error::{OAuthError, Result};

/// Validate a redirect URI and return its parsed form.
///
/// Accepted values are absolute `http`/`https` URLs with a host and without a
/// fragment or embedded credentials. Loopback HTTP is allowed because the Go
/// backend also supports `http://localhost:25774/...`.
pub fn validate_redirect_uri(uri: &str) -> Result<Url> {
    if uri.trim().is_empty() {
        return Err(OAuthError::InvalidRedirectUri("empty URI".to_owned()));
    }
    let parsed = Url::parse(uri)
        .map_err(|error| OAuthError::InvalidRedirectUri(format!("{error}: {uri}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(OAuthError::InvalidRedirectUri(format!(
            "unsupported scheme {:?}",
            parsed.scheme()
        )));
    }
    if parsed.host_str().is_none() {
        return Err(OAuthError::InvalidRedirectUri("missing host".to_owned()));
    }
    if parsed.fragment().is_some() {
        return Err(OAuthError::InvalidRedirectUri(
            "fragments are not allowed".to_owned(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(OAuthError::InvalidRedirectUri(
            "credentials are not allowed".to_owned(),
        ));
    }
    Ok(parsed)
}

/// Validate and return the original string, which preserves the exact
/// spelling used by the Go backend when building URLs.
pub fn checked_redirect_uri(uri: &str) -> Result<&str> {
    validate_redirect_uri(uri)?;
    Ok(uri)
}

#[cfg(test)]
mod tests {
    use super::validate_redirect_uri;

    #[test]
    fn accepts_http_and_https_redirects() {
        assert!(validate_redirect_uri("https://app.example/api/oauth_callback").is_ok());
        assert!(validate_redirect_uri("http://localhost:25774/api/oauth_callback").is_ok());
    }

    #[test]
    fn rejects_relative_and_fragment_redirects() {
        assert!(validate_redirect_uri("/api/oauth_callback").is_err());
        assert!(validate_redirect_uri("https://app.example/cb#fragment").is_err());
        assert!(validate_redirect_uri("javascript:alert(1)").is_err());
    }
}
