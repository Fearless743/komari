//! QQ aggregation-login provider.
//!
//! This is a direct port of Go's `utils/oauth/qq`. The aggregation platform
//! (`https://login.qjqq.cn` by default) returns a redirect URL from
//! `connect.php?act=login` and user information from
//! `connect.php?act=callback`. Both responses use the same `{code, msg}`
//! error convention.

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{OAuthError, Result, debug_secret, redact_secrets};
use crate::http::{json_response, string_field, value_to_string};
use crate::provider::OAuthProvider;
use crate::redirect::checked_redirect_uri;
use crate::state::StateStore;
use crate::types::{AuthorizationRequest, ExchangeRequest, TokenSet, UserInfo, sanitize_raw};

/// QQ configuration, matching Go's `qq.Addition`.
///
/// Go stores the zero value (`""`) when no configuration has been saved; the
/// `https://login.qjqq.cn` value in the factory metadata is only the UI
/// suggestion from the Go struct tag.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QqConfig {
    pub aggregation_url: String,
    pub app_id: String,
    pub app_key: String,
    pub login_type: String,
}

impl fmt::Debug for QqConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QqConfig")
            .field("aggregation_url", &self.aggregation_url)
            .field("app_id", &self.app_id)
            .field("app_key", &debug_secret(&self.app_key))
            .field("login_type", &self.login_type)
            .finish()
    }
}

/// QQ provider instance.
pub struct QqProvider {
    config: QqConfig,
    http: reqwest::Client,
    states: StateStore,
}

impl QqProvider {
    pub fn new(config: QqConfig, http: reqwest::Client) -> Self {
        Self {
            config,
            http,
            states: StateStore::default(),
        }
    }

    fn secrets(&self) -> Vec<&str> {
        vec![&self.config.app_key, &self.config.app_id]
    }

    fn base_url(&self) -> &str {
        self.config.aggregation_url.trim_end_matches('/')
    }
}

#[async_trait]
impl OAuthProvider for QqProvider {
    fn name(&self) -> &'static str {
        "qq"
    }

    fn configuration(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    fn state_store(&self) -> &StateStore {
        &self.states
    }

    fn skip_query_state_check(&self) -> bool {
        // The aggregation platform does not reliably preserve the query state
        // (Go's `providersSkipStateCheck`).
        true
    }

    async fn init(&self) -> Result<()> {
        if self.config.aggregation_url.trim().is_empty() {
            return Err(OAuthError::InvalidConfig(
                "aggregation_url is required".to_owned(),
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
        let request_url = format!(
            "{}/connect.php?act=login&appid={}&appkey={}&type={}&redirect_uri={}",
            self.base_url(),
            encode(&self.config.app_id),
            encode(&self.config.app_key),
            encode(&self.config.login_type),
            encode(redirect_uri),
        );

        let response = self.http.get(request_url).send().await.map_err(|error| {
            OAuthError::Http(redact_secrets(&error.to_string(), &self.secrets()))
        })?;
        let value = json_response(response, &self.secrets()).await?;
        let code = value.get("code").and_then(Value::as_i64).unwrap_or(-1);
        let message = string_field(&value, "msg").unwrap_or_else(|| "unknown error".to_owned());
        if code != 0 {
            return Err(OAuthError::provider_error(code.to_string(), message, None));
        }
        let url = string_field(&value, "url").ok_or_else(|| {
            OAuthError::InvalidResponse("QQ login response has no url".to_owned())
        })?;

        // Go registers the state only after the aggregation platform returns a
        // usable redirect URL.
        let state = self.states.issue(redirect_uri, None, None);
        Ok(AuthorizationRequest {
            url,
            state,
            pkce_verifier: None,
        })
    }

    async fn exchange_code(&self, request: ExchangeRequest<'_>) -> Result<TokenSet> {
        let login_type = request
            .query
            .get("type")
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or_else(|| self.config.login_type.clone());
        let callback_url = format!(
            "{}/connect.php?act=callback&appid={}&appkey={}&type={}&code={}",
            self.base_url(),
            encode(&self.config.app_id),
            encode(&self.config.app_key),
            encode(&login_type),
            encode(request.code),
        );

        let response = self.http.get(callback_url).send().await.map_err(|error| {
            OAuthError::UserInfo(redact_secrets(&error.to_string(), &self.secrets()))
        })?;
        let value = json_response(response, &self.secrets()).await?;
        let code = value.get("code").and_then(Value::as_i64).unwrap_or(-1);
        let message = string_field(&value, "msg").unwrap_or_else(|| "unknown error".to_owned());
        if code != 0 {
            return Err(OAuthError::provider_error(code.to_string(), message, None));
        }

        if subject_from(&value).is_none() {
            return Err(OAuthError::UserInfo("empty social_uid returned".to_owned()));
        }

        Ok(TokenSet {
            access_token: string_field(&value, "access_token").unwrap_or_default(),
            token_type: "Bearer".to_owned(),
            refresh_token: None,
            expires_in: None,
            expires_at: None,
            scope: None,
            id_token: None,
            raw: Some(sanitize_raw(value)),
            id_token_claims: None,
        })
    }

    async fn fetch_user_info(&self, tokens: &TokenSet) -> Result<UserInfo> {
        let value = tokens
            .raw
            .as_ref()
            .ok_or_else(|| OAuthError::UserInfo("QQ callback payload is missing".to_owned()))?;
        let subject = subject_from(value)
            .ok_or_else(|| OAuthError::UserInfo("empty social_uid returned".to_owned()))?;
        Ok(UserInfo {
            subject,
            username: string_field(value, "nickname"),
            email: None,
            email_verified: None,
            avatar: string_field(value, "faceimg"),
            union_id: string_field(value, "unionid"),
            open_id: string_field(value, "openid"),
            raw: Some(sanitize_raw(value.clone())),
        })
    }
}

fn subject_from(value: &Value) -> Option<String> {
    // Go only accepts social_uid. openid/unionid are retained as metadata but
    // must never become the account subject.
    string_field(value, "social_uid")
        .or_else(|| value.get("social_uid").and_then(value_to_string))
        .filter(|subject| !subject.is_empty())
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
