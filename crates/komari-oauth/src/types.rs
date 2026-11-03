//! Shared data structures exchanged between providers, the factory and the
//! account linker.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::accounts::SsoIdentity;

/// The result of building an authorization URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizationRequest {
    /// URL the browser must be redirected to.
    pub url: String,
    /// Random `state` value that must be verified on the callback.
    pub state: String,
    /// PKCE verifier when the provider uses PKCE.
    pub pkce_verifier: Option<String>,
}

/// Tokens returned by a token endpoint (or, for Cloudflare Access, the access
/// JWT itself).
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenSet {
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub token_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    /// Provider-specific payload kept in memory only. It is never serialized
    /// or logged.
    #[serde(skip)]
    pub raw: Option<serde_json::Value>,
    /// Verified `id_token` claims, kept in memory only.
    #[serde(skip)]
    pub id_token_claims: Option<serde_json::Value>,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenSet")
            .field(
                "access_token",
                &crate::error::debug_secret(&self.access_token),
            )
            .field("token_type", &self.token_type)
            .field(
                "refresh_token",
                &self
                    .refresh_token
                    .as_deref()
                    .map(crate::error::debug_secret),
            )
            .field("expires_in", &self.expires_in)
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .field(
                "id_token",
                &self.id_token.as_deref().map(crate::error::debug_secret),
            )
            .finish_non_exhaustive()
    }
}

impl TokenSet {
    /// Attach an absolute expiry based on `expires_in`.
    pub fn with_expiry_from_now(mut self) -> Self {
        if let Some(seconds) = self.expires_in {
            self.expires_at = Some(Utc::now() + Duration::seconds(seconds as i64));
        }
        self
    }

    /// Whether the access token is expired (with a 30 second safety margin).
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(expires_at) => expires_at <= Utc::now() + Duration::seconds(30),
            None => false,
        }
    }

    /// Whether a refresh token is available.
    pub fn can_refresh(&self) -> bool {
        self.refresh_token
            .as_deref()
            .is_some_and(|token| !token.is_empty())
    }
}

/// Provider-neutral user information.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UserInfo {
    /// Provider-specific immutable subject identifier.
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_verified: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// QQ union id / OpenID Connect `union_id` when the provider returns one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub union_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_id: Option<String>,
    /// Raw provider payload, useful to the API layer for display purposes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

/// A user resolved by a provider callback.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OAuthUser {
    /// Registered provider name (`generic`, `github`, `qq` or
    /// `CloudflareAccess`), matching the Go factory names.
    pub provider: String,
    /// Provider-specific immutable subject.
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_verified: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub union_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_id: Option<String>,
    /// Tokens obtained during the callback. They are kept in memory so the
    /// API layer can (for example) refresh a token, but they are not
    /// serialized by default.
    #[serde(skip)]
    pub tokens: Option<TokenSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

impl OAuthUser {
    /// Build a user from a provider's user-info response.
    pub fn from_user_info(
        provider: impl Into<String>,
        info: UserInfo,
        tokens: Option<TokenSet>,
    ) -> Self {
        Self {
            provider: provider.into(),
            subject: info.subject,
            username: info.username,
            email: info.email,
            email_verified: info.email_verified,
            avatar: info.avatar,
            union_id: info.union_id,
            open_id: info.open_id,
            tokens,
            raw: info.raw,
        }
    }

    /// Identity used by the account linker. The combined `sso_id` matches the
    /// Go backend's `fmt.Sprintf("%s_%s", providerName, userId)` convention.
    pub fn identity(&self) -> SsoIdentity {
        SsoIdentity::new(self.provider.clone(), self.subject.clone())
    }
}

/// Query parameters extracted from an OAuth callback.
pub type CallbackQuery = HashMap<String, String>;

/// Input passed to [`crate::OAuthProvider::callback`].
pub struct CallbackRequest<'a> {
    /// `state` read from the cookie (Go's `oauth_state` cookie).
    pub state: &'a str,
    /// `state` query parameter, when the provider sends one.
    pub query_state: Option<&'a str>,
    /// Authorization code (or access JWT for Cloudflare Access).
    pub code: Option<&'a str>,
    /// Callback URI used for this request.
    pub redirect_uri: &'a str,
    /// All callback query parameters.
    pub query: &'a CallbackQuery,
    /// Callback HTTP headers (Cloudflare Access reads
    /// `Cf-Access-Jwt-Assertion` from here).
    pub headers: &'a HeaderMap,
}

/// Input passed to [`crate::OAuthProvider::exchange_code`].
pub struct ExchangeRequest<'a> {
    pub code: &'a str,
    pub redirect_uri: &'a str,
    pub pkce_verifier: Option<&'a str>,
    pub nonce: Option<&'a str>,
    pub query: &'a CallbackQuery,
}
