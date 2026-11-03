//! Generic OAuth2 / OpenID Connect provider.
//!
//! The configuration fields and the default authorization URL layout match
//! Go's `utils/oauth/generic`. Optional OIDC discovery and `id_token`
//! validation are enabled by setting `issuer` (and optionally `jwks_url`).

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{OAuthError, Result, debug_secret, redact_secrets};
use crate::http::{json_response, parse_token_set, string_field, value_to_string};
use crate::jwt::{JwtVerifier, VerifiedJwt};
use crate::provider::OAuthProvider;
use crate::redirect::checked_redirect_uri;
use crate::state::{Pkce, StateStore, random_string};
use crate::types::{
    AuthorizationRequest, CallbackQuery, ExchangeRequest, TokenSet, UserInfo, sanitize_raw,
};

/// Configuration for the `generic` provider. The first seven fields are the
/// exact Go `generic.Addition` fields; the remaining fields are optional
/// extensions for OpenID Connect.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GenericConfig {
    pub client_id: String,
    pub client_secret: String,
    pub auth_url: String,
    pub token_url: String,
    pub user_info_url: String,
    pub scope: String,
    pub user_id_field: String,

    /// OIDC issuer used for discovery and `id_token` validation.
    pub issuer: String,
    /// Override the JWKS URL discovered from the issuer.
    pub jwks_url: String,
    /// Reject a token response without an `id_token`.
    pub id_token_required: bool,
    /// Send a PKCE `code_challenge` (disabled by default to match Go).
    pub use_pkce: bool,
    /// Send an OIDC `nonce` and verify it in the `id_token`.
    pub use_nonce: bool,
}

impl fmt::Debug for GenericConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GenericConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &debug_secret(&self.client_secret))
            .field("auth_url", &self.auth_url)
            .field("token_url", &self.token_url)
            .field("user_info_url", &self.user_info_url)
            .field("scope", &self.scope)
            .field("user_id_field", &self.user_id_field)
            .field("issuer", &self.issuer)
            .field("jwks_url", &self.jwks_url)
            .field("id_token_required", &self.id_token_required)
            .field("use_pkce", &self.use_pkce)
            .field("use_nonce", &self.use_nonce)
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct OidcDiscovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
    #[serde(default)]
    pub jwks_uri: Option<String>,
    #[serde(default)]
    pub end_session_endpoint: Option<String>,
}

/// Generic provider instance.
pub struct GenericProvider {
    config: GenericConfig,
    http: reqwest::Client,
    states: StateStore,
    discovery: RwLock<Option<OidcDiscovery>>,
    verifier: RwLock<Option<Arc<JwtVerifier>>>,
}

impl GenericProvider {
    pub fn new(config: GenericConfig, http: reqwest::Client) -> Self {
        Self {
            config,
            http,
            states: StateStore::default(),
            discovery: RwLock::new(None),
            verifier: RwLock::new(None),
        }
    }

    fn secrets(&self) -> Vec<&str> {
        vec![&self.config.client_secret, &self.config.client_id]
    }

    fn discovery(&self) -> Option<OidcDiscovery> {
        self.discovery.read().clone()
    }

    fn auth_endpoint(&self) -> Result<String> {
        if !self.config.auth_url.is_empty() {
            return Ok(self.config.auth_url.clone());
        }
        self.discovery()
            .map(|discovery| discovery.authorization_endpoint)
            .filter(|endpoint| !endpoint.is_empty())
            .ok_or_else(|| OAuthError::InvalidConfig("auth_url is required".to_owned()))
    }

    fn token_endpoint(&self) -> Result<String> {
        if !self.config.token_url.is_empty() {
            return Ok(self.config.token_url.clone());
        }
        self.discovery()
            .map(|discovery| discovery.token_endpoint)
            .filter(|endpoint| !endpoint.is_empty())
            .ok_or_else(|| OAuthError::InvalidConfig("token_url is required".to_owned()))
    }

    fn user_info_endpoint(&self) -> Option<String> {
        if !self.config.user_info_url.is_empty() {
            return Some(self.config.user_info_url.clone());
        }
        self.discovery()
            .and_then(|discovery| discovery.userinfo_endpoint)
            .filter(|endpoint| !endpoint.is_empty())
    }

    fn jwks_url(&self) -> Option<String> {
        if !self.config.jwks_url.is_empty() {
            return Some(self.config.jwks_url.clone());
        }
        self.discovery()
            .and_then(|discovery| discovery.jwks_uri)
            .filter(|url| !url.is_empty())
    }

    fn issuer(&self) -> Option<String> {
        if !self.config.issuer.is_empty() {
            return Some(self.config.issuer.clone());
        }
        self.discovery()
            .map(|discovery| discovery.issuer)
            .filter(|issuer| !issuer.is_empty())
    }

    /// Validate an `id_token` only when the operator opted into OIDC
    /// validation (issuer/JWKS configured) or explicitly required it.
    fn should_validate_id_token(&self) -> bool {
        self.config.id_token_required
            || !self.config.issuer.is_empty()
            || !self.config.jwks_url.is_empty()
    }

    /// All discovery-provided values are already configured explicitly.
    fn has_explicit_oidc_config(&self) -> bool {
        !self.config.issuer.is_empty()
            && !self.config.auth_url.is_empty()
            && !self.config.token_url.is_empty()
            && !self.config.user_info_url.is_empty()
            && !self.config.jwks_url.is_empty()
    }

    fn verifier(&self) -> Result<Arc<JwtVerifier>> {
        let jwks_url = self.jwks_url().ok_or_else(|| {
            OAuthError::InvalidConfig("jwks_url is required for id_token validation".to_owned())
        })?;
        let mut guard = self.verifier.write();
        if let Some(verifier) = guard.as_ref() {
            if verifier.jwks_url() == jwks_url {
                return Ok(Arc::clone(verifier));
            }
        }
        let verifier = Arc::new(JwtVerifier::new(
            self.http.clone(),
            jwks_url,
            self.issuer(),
            Some(self.config.client_id.clone()),
        ));
        *guard = Some(Arc::clone(&verifier));
        Ok(verifier)
    }

    async fn validate_id_token(&self, id_token: &str, nonce: Option<&str>) -> Result<VerifiedJwt> {
        let verifier = self.verifier()?;
        let verified = verifier.verify(id_token).await?;
        if let Some(expected) = nonce {
            if verified.claims.nonce.as_deref() != Some(expected) {
                return Err(OAuthError::InvalidToken(
                    "nonce mismatch in id_token".to_owned(),
                ));
            }
        }
        Ok(verified)
    }

    fn subject_from(&self, value: &Value) -> Result<String> {
        if self.config.user_id_field.trim().is_empty() {
            return Err(OAuthError::InvalidConfig(
                "user_id_field is required".to_owned(),
            ));
        }
        let field = value.get(&self.config.user_id_field).ok_or_else(|| {
            OAuthError::UserInfo(format!(
                "user id field '{}' not found in user info response",
                self.config.user_id_field
            ))
        })?;
        value_to_string(field).ok_or_else(|| {
            OAuthError::UserInfo(format!(
                "user id field '{}' is null",
                self.config.user_id_field
            ))
        })
    }
}

#[async_trait]
impl OAuthProvider for GenericProvider {
    fn name(&self) -> &'static str {
        "generic"
    }

    fn configuration(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    fn state_store(&self) -> &StateStore {
        &self.states
    }

    async fn init(&self) -> Result<()> {
        if self.config.user_id_field.trim().is_empty() {
            return Err(OAuthError::InvalidConfig(
                "user_id_field is required".to_owned(),
            ));
        }
        if self.config.use_nonce && !self.should_validate_id_token() {
            return Err(OAuthError::InvalidConfig(
                "use_nonce requires issuer, jwks_url or id_token_required".to_owned(),
            ));
        }
        if self.config.issuer.is_empty() {
            if self.config.id_token_required && self.jwks_url().is_none() {
                return Err(OAuthError::InvalidConfig(
                    "jwks_url is required when id_token_required is enabled".to_owned(),
                ));
            }
            return Ok(());
        }

        // Explicit endpoints and JWKS mean discovery is unnecessary. This is
        // important for providers that do not expose a well-known document.
        if self.has_explicit_oidc_config() {
            return Ok(());
        }

        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            self.config.issuer.trim_end_matches('/')
        );
        let response = self
            .http
            .get(&discovery_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| {
                OAuthError::Http(redact_secrets(&error.to_string(), &self.secrets()))
            })?;
        let value = json_response(response, &self.secrets()).await?;
        let discovery: OidcDiscovery = serde_json::from_value(value).map_err(|error| {
            OAuthError::InvalidConfig(format!("invalid OIDC discovery document: {error}"))
        })?;
        if discovery.issuer != self.config.issuer {
            return Err(OAuthError::InvalidConfig(format!(
                "OIDC discovery issuer mismatch: expected {:?}, got {:?}",
                self.config.issuer, discovery.issuer
            )));
        }
        *self.discovery.write() = Some(discovery);
        Ok(())
    }

    async fn destroy(&self) -> Result<()> {
        self.states.clear();
        *self.discovery.write() = None;
        *self.verifier.write() = None;
        Ok(())
    }

    async fn authorization_url(&self, redirect_uri: &str) -> Result<AuthorizationRequest> {
        checked_redirect_uri(redirect_uri)?;
        let auth_url = self.auth_endpoint()?;

        let pkce = if self.config.use_pkce {
            Some(Pkce::generate())
        } else {
            None
        };
        let nonce = if self.config.use_nonce {
            Some(random_string(16))
        } else {
            None
        };
        let state = self.states.issue(
            redirect_uri,
            pkce.as_ref().map(|pkce| pkce.verifier.clone()),
            nonce.clone(),
        );

        // Keep the parameter order of the Go provider exactly.
        let mut url = format!(
            "{}?client_id={}&state={}&scope={}&redirect_uri={}&response_type=code",
            auth_url,
            encode(&self.config.client_id),
            encode(&state),
            encode(&self.config.scope),
            encode(redirect_uri),
        );
        if let Some(nonce) = nonce {
            url.push_str("&nonce=");
            url.push_str(&encode(&nonce));
        }
        if let Some(pkce) = pkce.as_ref() {
            url.push_str("&code_challenge=");
            url.push_str(&encode(&pkce.challenge));
            url.push_str("&code_challenge_method=S256");
        }

        Ok(AuthorizationRequest {
            url,
            state,
            pkce_verifier: pkce.map(|pkce| pkce.verifier),
        })
    }

    async fn exchange_code(&self, request: ExchangeRequest<'_>) -> Result<TokenSet> {
        let token_url = self.token_endpoint()?;
        let mut form = vec![
            ("client_id", self.config.client_id.as_str()),
            ("client_secret", self.config.client_secret.as_str()),
            ("code", request.code),
            ("redirect_uri", request.redirect_uri),
            ("grant_type", "authorization_code"),
        ];
        if let Some(verifier) = request.pkce_verifier {
            form.push(("code_verifier", verifier));
        }

        let response = self
            .http
            .post(token_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&form)
            .send()
            .await
            .map_err(|error| {
                OAuthError::TokenExchange(redact_secrets(&error.to_string(), &self.secrets()))
            })?;
        let value = json_response(response, &self.secrets()).await?;
        let mut tokens = parse_token_set(&value, &self.secrets())?;

        let id_token = tokens.id_token.clone();
        match id_token {
            Some(id_token) if self.should_validate_id_token() => {
                let verified = self.validate_id_token(&id_token, request.nonce).await?;
                tokens.id_token_claims = Some(verified.payload);
            }
            Some(_) => {
                // Go's generic provider ignores id_token. Do not retain an
                // unverified token in memory.
                tokens.id_token = None;
            }
            None if self.config.id_token_required => {
                return Err(OAuthError::InvalidToken(
                    "token response did not contain an id_token".to_owned(),
                ));
            }
            None => {}
        }
        Ok(tokens)
    }

    async fn fetch_user_info(&self, tokens: &TokenSet) -> Result<UserInfo> {
        let value = if let Some(endpoint) = self.user_info_endpoint() {
            let response = self
                .http
                .get(endpoint)
                .header(reqwest::header::ACCEPT, "application/json")
                .bearer_auth(&tokens.access_token)
                .send()
                .await
                .map_err(|error| {
                    OAuthError::UserInfo(redact_secrets(&error.to_string(), &self.secrets()))
                })?;
            json_response(response, &self.secrets()).await?
        } else if let Some(claims) = &tokens.id_token_claims {
            claims.clone()
        } else {
            return Err(OAuthError::UserInfo(
                "user_info_url is required unless an id_token is returned".to_owned(),
            ));
        };

        let subject = self.subject_from(&value)?;
        Ok(UserInfo {
            subject,
            username: first_string(&value, &["name", "preferred_username", "nickname", "login"]),
            email: string_field(&value, "email"),
            email_verified: value.get("email_verified").and_then(Value::as_bool),
            avatar: first_string(&value, &["picture", "avatar_url", "avatar"]),
            union_id: string_field(&value, "union_id"),
            open_id: string_field(&value, "open_id"),
            raw: Some(sanitize_raw(value)),
        })
    }

    async fn refresh_token(&self, refresh_token: &str) -> Result<TokenSet> {
        let token_url = self.token_endpoint()?;
        let form = [
            ("client_id", self.config.client_id.as_str()),
            ("client_secret", self.config.client_secret.as_str()),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ];
        let response = self
            .http
            .post(token_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&form)
            .send()
            .await
            .map_err(|error| {
                OAuthError::TokenExchange(redact_secrets(&error.to_string(), &self.secrets()))
            })?;
        let value = json_response(response, &self.secrets()).await?;
        parse_token_set(&value, &self.secrets())
    }
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| string_field(value, key))
}

/// Parse a callback query map for tests and the API layer.
pub fn query_code(query: &CallbackQuery) -> Option<&str> {
    query.get("code").map(String::as_str)
}
