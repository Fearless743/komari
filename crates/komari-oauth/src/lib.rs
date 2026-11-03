//! Core OAuth2/OIDC building blocks for Komari.
//!
//! This commit introduces the provider trait, typed errors, state/PKCE
//! handling, redirect URI validation and JWT/JWKS verification. The built-in
//! providers, factory and account linker are added in the next commit.

pub mod accounts;
pub mod error;
pub mod http;
pub mod jwt;
pub mod provider;
pub mod redirect;
pub mod state;
pub mod types;

pub use accounts::{AccountLinker, OAuthAccount, SessionUser, SsoIdentity};
pub use error::{OAuthError, Result, redact_secrets, redacted_error};
pub use provider::OAuthProvider;
pub use redirect::{checked_redirect_uri, validate_redirect_uri};
pub use state::{Pkce, StateEntry, StateStore, random_string};
pub use types::{
    AuthorizationRequest, CallbackQuery, CallbackRequest, ExchangeRequest, OAuthUser, TokenSet,
    UserInfo,
};
