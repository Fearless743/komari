//! OAuth2 / OpenID Connect providers and external-account binding for Komari.
//!
//! This crate is the Rust port of the Go backend's `utils/oauth/**` packages
//! plus the account-linking parts of `api/public/oauth.go`:
//!
//! * [`OAuthProvider`] — the asynchronous equivalent of
//!   `factory.IOidcProvider` (authorization URL, token exchange, user info,
//!   refresh, state/PKCE, typed errors).
//! * [`factory`] — provider registry and configuration metadata matching Go's
//!   `factory` package (`generic`, `github`, `qq`, `CloudflareAccess`).
//! * [`providers`] — the four built-in providers. Endpoints, scopes and request
//!   parameters follow the Go implementations exactly by default.
//! * [`AccountLinker`] — looks up/creates accounts by `sso_type` + `sso_id`,
//!   binds an external identity without touching an existing local password
//!   and returns the information needed to create a session.
//! * [`StateStore`] — single-use CSRF state values with a five minute TTL and
//!   optional PKCE.
//!
//! Secrets are never included in error messages: provider errors pass through
//! [`redact_secrets`] before they are returned or logged.
//!
//! ```no_run
//! use komari_oauth::factory;
//!
//! # async fn example() -> komari_oauth::Result<()> {
//! let provider = factory::load_provider(
//!     "github",
//!     r#"{"client_id":"id","client_secret":"secret"}"#,
//! )
//! .await?;
//! let request = provider
//!     .authorization_url("https://monitor.example/api/oauth_callback")
//!     .await?;
//! println!("redirect the browser to {}", request.url);
//! # Ok(())
//! # }
//! ```

pub mod accounts;
pub mod error;
pub mod factory;
pub mod http;
pub mod jwt;
pub mod manager;
pub mod provider;
pub mod providers;
pub mod redirect;
pub mod state;
pub mod types;

pub use accounts::{AccountLinker, OAuthAccount, SessionUser, SsoIdentity};
pub use error::{OAuthError, Result, redact_secrets, redacted_error};
pub use factory::{
    BUILTIN_METADATA, ConfigField, ProviderConstructor, ProviderMeta, ProviderRegistry,
    all_provider_names, builtin_registry, default_config, default_http_client, load_provider,
};
pub use manager::{
    DEFAULT_PROVIDER, OAUTH_ENABLED_KEY, OAUTH_PROVIDER_KEY, OAuthManager, current_provider,
    global, load_provider_global,
};
pub use provider::OAuthProvider;
pub use providers::{CloudflareConfig, GenericConfig, GithubConfig, QqConfig};
pub use redirect::{
    RedirectPolicy, checked_redirect_uri, checked_redirect_uri_for_base, validate_redirect_uri,
    validate_redirect_uri_for_base,
};
pub use state::{Pkce, StateEntry, StateStore, random_string};
pub use types::{
    AuthorizationRequest, CallbackQuery, CallbackRequest, ExchangeRequest, OAuthUser, TokenSet,
    UserInfo,
};
