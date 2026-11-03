//! Redirect URI validation.
//!
//! Go simply interpolates the callback URI into the authorization URL. The
//! Rust port rejects malformed values and non-loopback plain HTTP. API layers
//! must additionally bind the callback to the configured public base URL with
//! [`RedirectPolicy`] (or [`validate_redirect_uri_for_base`]); validating only
//! the shape cannot tell `evil.example` from the real callback host.

use url::Url;

use crate::error::{OAuthError, Result};

/// Validate a redirect URI and return its parsed form.
///
/// Accepted values are absolute `https` URLs (or loopback `http` URLs) with a
/// host and without a fragment or embedded credentials.
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
    let Some(host) = parsed.host_str() else {
        return Err(OAuthError::InvalidRedirectUri("missing host".to_owned()));
    };
    if parsed.scheme() == "http" && !is_loopback(host) {
        return Err(OAuthError::InvalidRedirectUri(
            "plain HTTP is only allowed for loopback hosts".to_owned(),
        ));
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

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "::1"
        || host == "[::1]"
}

/// Validate that `uri` has the same scheme, host, port and path as the
/// configured callback base URL.
///
/// Query parameters are allowed (Cloudflare Access appends `state` to the
/// callback URL); fragments and credentials are still rejected.
pub fn validate_redirect_uri_for_base(uri: &str, callback_base: &str) -> Result<Url> {
    let parsed = validate_redirect_uri(uri)?;
    let base = validate_redirect_uri(callback_base)?;
    if parsed.scheme() != base.scheme()
        || parsed.host_str() != base.host_str()
        || parsed.port_or_known_default() != base.port_or_known_default()
        || parsed.path() != base.path()
    {
        return Err(OAuthError::InvalidRedirectUri(format!(
            "redirect URI {uri:?} does not match configured callback {callback_base:?}"
        )));
    }
    Ok(parsed)
}

/// A redirect policy bound to the configured public callback URL.
#[derive(Clone, Debug)]
pub struct RedirectPolicy {
    base: Url,
}

impl RedirectPolicy {
    pub fn new(callback_base: impl AsRef<str>) -> Result<Self> {
        let base = validate_redirect_uri(callback_base.as_ref())?;
        Ok(Self { base })
    }

    pub fn validate(&self, uri: &str) -> Result<Url> {
        validate_redirect_uri_for_base(uri, self.base.as_str())
    }
}

/// Validate and return the original string, which preserves the exact
/// spelling used by the Go backend when building URLs.
pub fn checked_redirect_uri(uri: &str) -> Result<&str> {
    validate_redirect_uri(uri)?;
    Ok(uri)
}

/// Validate a callback URI against the configured public callback base.
pub fn checked_redirect_uri_for_base<'a>(uri: &'a str, callback_base: &str) -> Result<&'a str> {
    validate_redirect_uri_for_base(uri, callback_base)?;
    Ok(uri)
}

#[cfg(test)]
mod tests {
    use super::{RedirectPolicy, validate_redirect_uri, validate_redirect_uri_for_base};

    #[test]
    fn accepts_https_and_loopback_http_redirects() {
        assert!(validate_redirect_uri("https://app.example/api/oauth_callback").is_ok());
        assert!(validate_redirect_uri("http://localhost:25774/api/oauth_callback").is_ok());
        assert!(validate_redirect_uri("http://127.0.0.1:25774/cb").is_ok());
    }

    #[test]
    fn rejects_relative_fragment_and_plain_http_redirects() {
        assert!(validate_redirect_uri("/api/oauth_callback").is_err());
        assert!(validate_redirect_uri("https://app.example/cb#fragment").is_err());
        assert!(validate_redirect_uri("javascript:alert(1)").is_err());
        assert!(validate_redirect_uri("http://evil.example/cb").is_err());
    }

    #[test]
    fn binds_the_redirect_to_the_configured_callback_base() {
        let base = "https://monitor.example/api/oauth_callback";
        assert!(validate_redirect_uri_for_base(base, base).is_ok());
        assert!(
            validate_redirect_uri_for_base(
                "https://monitor.example/api/oauth_callback?state=x",
                base
            )
            .is_ok()
        );
        assert!(
            validate_redirect_uri_for_base("https://evil.example/api/oauth_callback", base)
                .is_err()
        );
        assert!(validate_redirect_uri_for_base("https://monitor.example/other", base).is_err());

        let policy = RedirectPolicy::new(base).unwrap();
        assert!(
            policy
                .validate("https://monitor.example/api/oauth_callback")
                .is_ok()
        );
        assert!(
            policy
                .validate("https://evil.example/api/oauth_callback")
                .is_err()
        );
    }
}
