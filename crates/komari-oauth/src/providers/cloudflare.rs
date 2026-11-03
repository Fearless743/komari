//! Cloudflare Access provider.
//!
//! Cloudflare Access does not use an authorization-code exchange. The
//! callback carries a signed JWT in the `Cf-Access-Jwt-Assertion` header (or,
//! for non-browser clients, in a `code`/`token` query parameter). The JWT is
//! verified against the team's `/cdn-cgi/access/certs` JWKS with `policy_aud`
//! as the expected audience.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{OAuthError, Result};
use crate::http::callback_error;
use crate::jwt::{JwtVerifier, VerifiedJwt};
use crate::provider::OAuthProvider;
use crate::redirect::checked_redirect_uri;
use crate::state::StateStore;
use crate::types::{
    AuthorizationRequest, CallbackRequest, ExchangeRequest, OAuthUser, TokenSet, UserInfo,
};

/// Cloudflare Access configuration, matching Go's `cloudflare.Addition`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudflareConfig {
    pub team_domain: String,
    pub policy_aud: String,
}

/// Cloudflare Access provider instance.
pub struct CloudflareProvider {
    config: CloudflareConfig,
    states: StateStore,
    verifier: JwtVerifier,
}

impl CloudflareProvider {
    pub fn new(config: CloudflareConfig, http: reqwest::Client) -> Self {
        let team_domain = normalize_team_domain(&config.team_domain);
        let verifier = JwtVerifier::new(
            http.clone(),
            format!("{team_domain}/cdn-cgi/access/certs"),
            Some(team_domain),
            Some(config.policy_aud.clone()),
        );
        Self {
            config,
            states: StateStore::default(),
            verifier,
        }
    }

    async fn verified_user(&self, jwt: &str) -> Result<VerifiedJwt> {
        self.verifier.verify(jwt).await
    }

    fn user_from_claims(&self, verified: VerifiedJwt) -> Result<OAuthUser> {
        let subject = verified
            .claims
            .sub
            .clone()
            .filter(|subject| !subject.is_empty())
            .ok_or_else(|| {
                OAuthError::UserInfo("Cloudflare Access token has no sub claim".to_owned())
            })?;
        Ok(OAuthUser {
            provider: self.name().to_owned(),
            subject,
            username: verified
                .claims
                .email
                .as_deref()
                .and_then(|email| email.split('@').next())
                .map(str::to_owned),
            email: verified.claims.email.clone(),
            email_verified: verified.claims.email_verified,
            avatar: None,
            union_id: None,
            open_id: None,
            tokens: None,
            raw: Some(verified.payload),
        })
    }
}

#[async_trait]
impl OAuthProvider for CloudflareProvider {
    fn name(&self) -> &'static str {
        "CloudflareAccess"
    }

    fn configuration(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    fn state_store(&self) -> &StateStore {
        &self.states
    }

    async fn init(&self) -> Result<()> {
        if self.config.team_domain.trim().is_empty() {
            return Err(OAuthError::InvalidConfig(
                "team_domain is required".to_owned(),
            ));
        }
        if self.config.policy_aud.trim().is_empty() {
            return Err(OAuthError::InvalidConfig(
                "policy_aud is required".to_owned(),
            ));
        }
        Ok(())
    }

    async fn destroy(&self) -> Result<()> {
        self.states.clear();
        Ok(())
    }

    async fn authorization_url(&self, redirect_uri: &str) -> Result<AuthorizationRequest> {
        checked_redirect_uri(redirect_uri)?;
        // Go uses an 8-character state for Cloudflare Access.
        let state = crate::state::random_string(8);
        self.states.issue_with_state(redirect_uri, &state);
        let separator = if redirect_uri.contains('?') { '&' } else { '?' };
        Ok(AuthorizationRequest {
            url: format!("{redirect_uri}{separator}state={state}"),
            state,
            pkce_verifier: None,
        })
    }

    async fn exchange_code(&self, request: ExchangeRequest<'_>) -> Result<TokenSet> {
        let verified = self.verified_user(request.code).await?;
        Ok(TokenSet {
            access_token: request.code.to_owned(),
            token_type: "Bearer".to_owned(),
            refresh_token: None,
            expires_in: None,
            expires_at: None,
            scope: None,
            id_token: Some(request.code.to_owned()),
            raw: Some(verified.payload.clone()),
            id_token_claims: Some(verified.payload),
        })
    }

    async fn fetch_user_info(&self, tokens: &TokenSet) -> Result<UserInfo> {
        let token = if !tokens.access_token.is_empty() {
            tokens.access_token.as_str()
        } else {
            tokens.id_token.as_deref().unwrap_or_default()
        };
        let verified = self.verified_user(token).await?;
        let subject = verified
            .claims
            .sub
            .clone()
            .filter(|subject| !subject.is_empty())
            .ok_or_else(|| {
                OAuthError::UserInfo("Cloudflare Access token has no sub claim".to_owned())
            })?;
        Ok(UserInfo {
            subject,
            username: verified
                .claims
                .email
                .as_deref()
                .and_then(|email| email.split('@').next())
                .map(str::to_owned),
            email: verified.claims.email.clone(),
            email_verified: verified.claims.email_verified,
            avatar: None,
            union_id: None,
            open_id: None,
            raw: Some(verified.payload),
        })
    }

    async fn callback(&self, request: CallbackRequest<'_>) -> Result<OAuthUser> {
        self.states.consume(request.state, request.redirect_uri)?;
        let query_state = request.query_state.unwrap_or_default();
        if query_state != request.state {
            return Err(OAuthError::InvalidState);
        }
        if let Some(error) = callback_error(request.query) {
            return Err(error);
        }

        let jwt = request
            .headers
            .get("Cf-Access-Jwt-Assertion")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                request
                    .query
                    .get("code")
                    .or_else(|| request.query.get("token"))
                    .or_else(|| request.query.get("access_token"))
                    .cloned()
            })
            .or_else(|| request.code.map(str::to_owned))
            .ok_or_else(|| OAuthError::UserInfo("no Cloudflare Access JWT found".to_owned()))?;

        let verified = self.verified_user(&jwt).await?;
        self.user_from_claims(verified)
    }
}

fn normalize_team_domain(team_domain: &str) -> String {
    let team_domain = team_domain.trim().trim_end_matches('/');
    if team_domain.starts_with("https://") || team_domain.starts_with("http://") {
        team_domain.to_owned()
    } else {
        format!("https://{team_domain}.cloudflareaccess.com")
    }
}
