//! Built-in provider implementations.

pub mod cloudflare;
pub mod generic;
pub mod github;
pub mod qq;

pub use cloudflare::{CloudflareConfig, CloudflareProvider};
pub use generic::{GenericConfig, GenericProvider, OidcDiscovery};
pub use github::{GithubConfig, GithubProvider};
pub use qq::{QqConfig, QqProvider};
