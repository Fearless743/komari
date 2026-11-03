//! Provider factory and registry.
//!
//! Go registers constructors in a package-level map (`factory.RegisterOidcProvider`)
//! and exposes the reflected configuration fields through `item.Parse`. The
//! Rust registry provides the same information explicitly via [`ProviderMeta`]
//! and [`ConfigField`].

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::{Value, json};

use crate::error::{OAuthError, Result};
use crate::provider::OAuthProvider;
use crate::providers::{
    CloudflareConfig, CloudflareProvider, GenericConfig, GenericProvider, GithubConfig,
    GithubProvider, QqConfig, QqProvider,
};

/// One configuration field exposed to the admin UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfigField {
    pub name: &'static str,
    pub required: bool,
    /// Default value serialized into a new provider row.
    pub default: Option<&'static str>,
    /// Whether the value must be masked in logs and API responses.
    pub secret: bool,
    pub description: &'static str,
}

/// Metadata for one registered provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderMeta {
    pub name: &'static str,
    pub fields: &'static [ConfigField],
}

pub const GENERIC_FIELDS: &[ConfigField] = &[
    ConfigField {
        name: "client_id",
        required: true,
        default: None,
        secret: false,
        description: "OAuth client id",
    },
    ConfigField {
        name: "client_secret",
        required: true,
        default: None,
        secret: true,
        description: "OAuth client secret",
    },
    ConfigField {
        name: "auth_url",
        required: true,
        default: None,
        secret: false,
        description: "Authorization endpoint",
    },
    ConfigField {
        name: "token_url",
        required: true,
        default: None,
        secret: false,
        description: "Token endpoint",
    },
    ConfigField {
        name: "user_info_url",
        required: true,
        default: None,
        secret: false,
        description: "User-info endpoint",
    },
    ConfigField {
        name: "scope",
        required: false,
        default: None,
        secret: false,
        description: "Space-separated OAuth scopes",
    },
    ConfigField {
        name: "user_id_field",
        required: true,
        default: None,
        secret: false,
        description: "JSON field used as the stable user id",
    },
];

pub const GITHUB_FIELDS: &[ConfigField] = &[
    ConfigField {
        name: "client_id",
        required: true,
        default: None,
        secret: false,
        description: "GitHub OAuth app client id",
    },
    ConfigField {
        name: "client_secret",
        required: true,
        default: None,
        secret: true,
        description: "GitHub OAuth app client secret",
    },
];

pub const QQ_FIELDS: &[ConfigField] = &[
    ConfigField {
        name: "aggregation_url",
        required: true,
        default: Some("https://login.qjqq.cn"),
        secret: false,
        description: "QQ aggregation login base URL",
    },
    ConfigField {
        name: "app_id",
        required: true,
        default: None,
        secret: false,
        description: "Aggregation app id",
    },
    ConfigField {
        name: "app_key",
        required: true,
        default: None,
        secret: true,
        description: "Aggregation app key",
    },
    ConfigField {
        name: "login_type",
        required: true,
        default: None,
        secret: false,
        description: "Login type (qq, google, ...)",
    },
];

pub const CLOUDFLARE_FIELDS: &[ConfigField] = &[
    ConfigField {
        name: "team_domain",
        required: true,
        default: None,
        secret: false,
        description: "Cloudflare Access team domain",
    },
    ConfigField {
        name: "policy_aud",
        required: true,
        default: None,
        secret: false,
        description: "Cloudflare Access application audience (AUD) tag",
    },
];

/// All built-in providers, in a stable order.
pub const BUILTIN_METADATA: &[ProviderMeta] = &[
    ProviderMeta {
        name: "generic",
        fields: GENERIC_FIELDS,
    },
    ProviderMeta {
        name: "github",
        fields: GITHUB_FIELDS,
    },
    ProviderMeta {
        name: "qq",
        fields: QQ_FIELDS,
    },
    ProviderMeta {
        name: "CloudflareAccess",
        fields: CLOUDFLARE_FIELDS,
    },
];

/// Constructor signature used by the registry.
pub type ProviderConstructor =
    fn(Value, reqwest::Client) -> Result<std::sync::Arc<dyn OAuthProvider>>;

/// A registry of provider constructors.
#[derive(Default)]
pub struct ProviderRegistry {
    constructors: HashMap<&'static str, ProviderConstructor>,
}

impl ProviderRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a registry with all built-in providers registered.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register("generic", generic_constructor);
        registry.register("github", github_constructor);
        registry.register("qq", qq_constructor);
        registry.register("CloudflareAccess", cloudflare_constructor);
        registry
    }

    /// Register or replace a provider constructor (Go
    /// `factory.RegisterOidcProvider`).
    pub fn register(&mut self, name: &'static str, constructor: ProviderConstructor) {
        self.constructors.insert(name, constructor);
    }

    /// Look up a constructor by name (Go `factory.GetConstructor`).
    pub fn constructor(&self, name: &str) -> Option<ProviderConstructor> {
        self.constructors.get(name).copied()
    }

    /// Registered names, sorted for deterministic output.
    pub fn names(&self) -> Vec<&'static str> {
        let mut names = self.constructors.keys().copied().collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    /// Metadata for every registered provider (Go `factory.GetProviderConfigs`).
    pub fn metadata(&self) -> Vec<&'static ProviderMeta> {
        let mut metadata = BUILTIN_METADATA
            .iter()
            .filter(|meta| self.constructors.contains_key(meta.name))
            .collect::<Vec<_>>();
        metadata.sort_by_key(|meta| meta.name);
        metadata
    }

    /// Configuration fields for one provider.
    pub fn config_fields(&self, name: &str) -> Option<&'static [ConfigField]> {
        BUILTIN_METADATA
            .iter()
            .find(|meta| meta.name == name)
            .map(|meta| meta.fields)
    }

    /// Instantiate a provider without calling `init`.
    pub fn create(&self, name: &str, config: Value) -> Result<std::sync::Arc<dyn OAuthProvider>> {
        let constructor = self
            .constructor(name)
            .ok_or_else(|| OAuthError::ProviderNotFound(name.to_owned()))?;
        constructor(config, default_http_client())
    }

    /// Instantiate and initialize a provider.
    pub async fn create_and_init(
        &self,
        name: &str,
        config: Value,
    ) -> Result<std::sync::Arc<dyn OAuthProvider>> {
        self.create_and_init_with_client(name, config, default_http_client())
            .await
    }

    /// Instantiate and initialize a provider with a caller-supplied HTTP
    /// client (useful for tests and connection-pool reuse).
    pub async fn create_and_init_with_client(
        &self,
        name: &str,
        config: Value,
        client: reqwest::Client,
    ) -> Result<std::sync::Arc<dyn OAuthProvider>> {
        let constructor = self
            .constructor(name)
            .ok_or_else(|| OAuthError::ProviderNotFound(name.to_owned()))?;
        let provider = constructor(config, client)?;
        provider.init().await?;
        Ok(provider)
    }
}

/// The process-wide registry with the four built-in providers.
pub fn builtin_registry() -> &'static ProviderRegistry {
    static REGISTRY: OnceLock<ProviderRegistry> = OnceLock::new();
    REGISTRY.get_or_init(ProviderRegistry::with_builtins)
}

/// Default HTTP client used by providers.
pub fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Names of all built-in providers (Go `factory.GetAllOidcProviderNames`).
pub fn all_provider_names() -> Vec<&'static str> {
    builtin_registry().names()
}

/// Build a provider from a JSON configuration (Go `oauth.LoadProvider`).
pub async fn load_provider(
    name: &str,
    config_json: &str,
) -> Result<std::sync::Arc<dyn OAuthProvider>> {
    let value = if config_json.trim().is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(config_json)
            .map_err(|error| OAuthError::InvalidConfig(error.to_string()))?
    };
    builtin_registry().create_and_init(name, value).await
}

/// Default configuration JSON stored when a provider row is first created.
pub fn default_config(name: &str) -> Value {
    match name {
        "generic" => json!({
            "client_id": "",
            "client_secret": "",
            "auth_url": "",
            "token_url": "",
            "user_info_url": "",
            "scope": "",
            "user_id_field": "",
        }),
        "github" => json!({
            "client_id": "",
            "client_secret": "",
        }),
        "qq" => json!({
            // Go's Initialize marshals the zero value; the metadata default
            // remains the UI suggestion from the struct tag.
            "aggregation_url": "",
            "app_id": "",
            "app_key": "",
            "login_type": "",
        }),
        "CloudflareAccess" => json!({
            "team_domain": "",
            "policy_aud": "",
        }),
        _ => json!({}),
    }
}

fn generic_constructor(
    value: Value,
    http: reqwest::Client,
) -> Result<std::sync::Arc<dyn OAuthProvider>> {
    let config: GenericConfig = serde_json::from_value(value)
        .map_err(|error| OAuthError::InvalidConfig(error.to_string()))?;
    Ok(std::sync::Arc::new(GenericProvider::new(config, http)))
}

fn github_constructor(
    value: Value,
    http: reqwest::Client,
) -> Result<std::sync::Arc<dyn OAuthProvider>> {
    let config: GithubConfig = serde_json::from_value(value)
        .map_err(|error| OAuthError::InvalidConfig(error.to_string()))?;
    Ok(std::sync::Arc::new(GithubProvider::new(config, http)))
}

fn qq_constructor(
    value: Value,
    http: reqwest::Client,
) -> Result<std::sync::Arc<dyn OAuthProvider>> {
    let config: QqConfig = serde_json::from_value(value)
        .map_err(|error| OAuthError::InvalidConfig(error.to_string()))?;
    Ok(std::sync::Arc::new(QqProvider::new(config, http)))
}

fn cloudflare_constructor(
    value: Value,
    http: reqwest::Client,
) -> Result<std::sync::Arc<dyn OAuthProvider>> {
    let config: CloudflareConfig = serde_json::from_value(value)
        .map_err(|error| OAuthError::InvalidConfig(error.to_string()))?;
    Ok(std::sync::Arc::new(CloudflareProvider::new(config, http)))
}
