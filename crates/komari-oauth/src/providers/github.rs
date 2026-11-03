//! GitHub OAuth2 provider.
//!
//! Endpoints, scopes and request parameters match Go's
//! `utils/oauth/github`: the authorization URL does not include a
//! `redirect_uri` (GitHub uses the value configured on the OAuth app) and the
//! token request sends its parameters in the query string.

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{OAuthError, Result, debug_secret, redact_secrets};
use crate::http::{json_response, parse_token_set, string_field, value_to_string};
use crate::provider::OAuthProvider;
use crate::redirect::checked_redirect_uri;
use crate::state::StateStore;
use crate::types::{AuthorizationRequest, ExchangeRequest, TokenSet, UserInfo, sanitize_raw};

const GITHUB_AUTHORIZE_URL: &str = "https://github.com/login/oauth/authorize";
const GITHUB_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_USER_URL: &str = "https://api.github.com/user";
const GITHUB_EMAILS_URL: &str = "https://api.github.com/user/emails";

/// GitHub configuration. The first two fields are Go's `github.Addition`; the
/// URL overrides exist for GitHub Enterprise and tests.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GithubConfig {
    pub client_id: String,
    pub client_secret: String,

    #[serde(default = "default_authorize_url")]
    pub authorize_url: String,
    #[serde(default = "default_token_url")]
    pub token_url: String,
    #[serde(default = "default_user_api_url")]
    pub user_api_url: String,
    #[serde(default = "default_email_api_url")]
    pub email_api_url: String,
    /// Fetch `/user/emails` when the user profile has no public email.
    #[serde(default = "default_true")]
    pub fetch_email: bool,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            authorize_url: GITHUB_AUTHORIZE_URL.to_owned(),
            token_url: GITHUB_TOKEN_URL.to_owned(),
            user_api_url: GITHUB_USER_URL.to_owned(),
            email_api_url: GITHUB_EMAILS_URL.to_owned(),
            fetch_email: true,
        }
    }
}

impl fmt::Debug for GithubConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GithubConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &debug_secret(&self.client_secret))
            .field("authorize_url", &self.authorize_url)
            .field("token_url", &self.token_url)
            .field("user_api_url", &self.user_api_url)
            .field("email_api_url", &self.email_api_url)
            .field("fetch_email", &self.fetch_email)
            .finish()
    }
}

/// GitHub provider instance.
pub struct GithubProvider {
    config: GithubConfig,
    http: reqwest::Client,
    states: StateStore,
}

impl GithubProvider {
    pub fn new(config: GithubConfig, http: reqwest::Client) -> Self {
        Self {
            config,
            http,
            states: StateStore::default(),
        }
    }

    fn secrets(&self) -> Vec<&str> {
        vec![&self.config.client_secret, &self.config.client_id]
    }

    async fn email_from_api(&self, access_token: &str) -> Option<String> {
        let response = self
            .http
            .get(&self.config.email_api_url)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .bearer_auth(access_token)
            .send()
            .await
            .ok()?;
        let value: Value = response.json().await.ok()?;
        let emails = value.as_array()?;
        let mut first_verified = None;
        let mut first_any = None;
        for email in emails {
            let Some(address) = string_field(email, "email") else {
                continue;
            };
            if first_any.is_none() {
                first_any = Some(address.clone());
            }
            let verified = email
                .get("verified")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let primary = email
                .get("primary")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if primary && verified {
                return Some(address);
            }
            if verified && first_verified.is_none() {
                first_verified = Some(address);
            }
        }
        first_verified.or(first_any)
    }
}

#[async_trait]
impl OAuthProvider for GithubProvider {
    fn name(&self) -> &'static str {
        "github"
    }

    fn configuration(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    fn state_store(&self) -> &StateStore {
        &self.states
    }

    async fn init(&self) -> Result<()> {
        Ok(())
    }

    async fn destroy(&self) -> Result<()> {
        self.states.clear();
        Ok(())
    }

    async fn authorization_url(&self, redirect_uri: &str) -> Result<AuthorizationRequest> {
        checked_redirect_uri(redirect_uri)?;
        let state = self.states.issue(redirect_uri, None, None);
        let url = format!(
            "{}?client_id={}&state={}&scope=user:email",
            self.config.authorize_url,
            encode(&self.config.client_id),
            encode(&state),
        );
        Ok(AuthorizationRequest {
            url,
            state,
            pkce_verifier: None,
        })
    }

    async fn exchange_code(&self, request: ExchangeRequest<'_>) -> Result<TokenSet> {
        let response = self
            .http
            .post(&self.config.token_url)
            .query(&[
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", self.config.client_secret.as_str()),
                ("code", request.code),
            ])
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| {
                OAuthError::TokenExchange(redact_secrets(&error.to_string(), &self.secrets()))
            })?;
        let value = json_response(response, &self.secrets()).await?;
        parse_token_set(&value, &self.secrets())
    }

    async fn fetch_user_info(&self, tokens: &TokenSet) -> Result<UserInfo> {
        let response = self
            .http
            .get(&self.config.user_api_url)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .bearer_auth(&tokens.access_token)
            .send()
            .await
            .map_err(|error| {
                OAuthError::UserInfo(redact_secrets(&error.to_string(), &self.secrets()))
            })?;
        let value = json_response(response, &self.secrets()).await?;

        let id = value
            .get("id")
            .and_then(value_to_string)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                OAuthError::UserInfo("GitHub user response did not contain an id".to_owned())
            })?;

        let email = match string_field(&value, "email") {
            Some(email) => Some(email),
            None if self.config.fetch_email => self.email_from_api(&tokens.access_token).await,
            None => None,
        };

        Ok(UserInfo {
            subject: id,
            username: string_field(&value, "login"),
            email,
            email_verified: None,
            avatar: string_field(&value, "avatar_url"),
            union_id: None,
            open_id: None,
            raw: Some(sanitize_raw(value)),
        })
    }
}

/// `fetch_user_info` is async but the email fallback is only used in one
/// branch; keeping it in a separate function avoids borrowing `self` across
/// the user response borrow.
fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn default_authorize_url() -> String {
    GITHUB_AUTHORIZE_URL.to_owned()
}

fn default_token_url() -> String {
    GITHUB_TOKEN_URL.to_owned()
}

fn default_user_api_url() -> String {
    GITHUB_USER_URL.to_owned()
}

fn default_email_api_url() -> String {
    GITHUB_EMAILS_URL.to_owned()
}

fn default_true() -> bool {
    true
}
