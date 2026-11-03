//! The provider trait and callback orchestration.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{OAuthError, Result};
use crate::http::callback_error;
use crate::state::StateStore;
use crate::types::{
    AuthorizationRequest, CallbackRequest, ExchangeRequest, OAuthUser, TokenSet, UserInfo,
};

/// An OAuth2/OIDC provider.
///
/// The interface mirrors Go's `factory.IOidcProvider` but is asynchronous
/// because token exchange, discovery and JWKS fetching perform HTTP requests.
#[async_trait]
pub trait OAuthProvider: Send + Sync {
    /// Registered name, matching the Go factory names exactly:
    /// `generic`, `github`, `qq`, `CloudflareAccess`.
    fn name(&self) -> &'static str;

    /// Current configuration as JSON (Go `GetConfiguration`). The returned
    /// value includes secrets and must not be logged.
    fn configuration(&self) -> Value;

    /// Store of pending `state` values for this provider instance.
    fn state_store(&self) -> &StateStore;

    /// QQ skips the cookie-vs-query state comparison because the aggregation
    /// platform does not preserve the query state (Go's
    /// `providersSkipStateCheck`).
    fn skip_query_state_check(&self) -> bool {
        false
    }

    /// Validate configuration and, for OIDC providers, fetch discovery
    /// metadata.
    async fn init(&self) -> Result<()>;

    /// Release cached state/JWKS data.
    async fn destroy(&self) -> Result<()> {
        Ok(())
    }

    /// Build the authorization URL and register a new `state` value.
    async fn authorization_url(&self, redirect_uri: &str) -> Result<AuthorizationRequest>;

    /// Exchange an authorization code for tokens.
    async fn exchange_code(&self, request: ExchangeRequest<'_>) -> Result<TokenSet>;

    /// Fetch provider-neutral user information.
    async fn fetch_user_info(&self, tokens: &TokenSet) -> Result<UserInfo>;

    /// Refresh an access token. Providers without refresh support return
    /// [`OAuthError::Unsupported`].
    async fn refresh_token(&self, _refresh_token: &str) -> Result<TokenSet> {
        Err(OAuthError::Unsupported(format!(
            "{} does not support token refresh",
            self.name()
        )))
    }

    /// Validate the callback state, exchange the code and map the user.
    ///
    /// Providers with a non-standard callback (Cloudflare Access) override
    /// this method.
    async fn callback(&self, request: CallbackRequest<'_>) -> Result<OAuthUser> {
        let entry = self
            .state_store()
            .consume(request.state, request.redirect_uri)?;

        if !self.skip_query_state_check() {
            let query_state = request.query_state.unwrap_or_default();
            if query_state != request.state {
                return Err(OAuthError::InvalidState);
            }
        }

        if let Some(error) = callback_error(request.query) {
            return Err(error);
        }

        let code = request
            .code
            .filter(|code| !code.is_empty())
            .ok_or(OAuthError::MissingCode)?;
        let tokens = self
            .exchange_code(ExchangeRequest {
                code,
                redirect_uri: request.redirect_uri,
                pkce_verifier: entry.pkce_verifier.as_deref(),
                nonce: entry.nonce.as_deref(),
                query: request.query,
            })
            .await?;
        let info = self.fetch_user_info(&tokens).await?;
        if info.subject.is_empty() {
            return Err(OAuthError::UserInfo(
                "provider returned an empty user identifier".to_owned(),
            ));
        }
        Ok(OAuthUser::from_user_info(self.name(), info, Some(tokens)))
    }
}
