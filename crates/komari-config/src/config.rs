use std::{env, fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{LogLevel, keys};

/// Default listen address, matching the Go backend.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:25774";
/// Default SQLite database file, matching the Go backend.
pub const DEFAULT_DB_FILE: &str = "data/komari.db";

/// Supported database backends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseType {
    #[default]
    Sqlite,
    Mysql,
}

impl DatabaseType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Mysql => "mysql",
        }
    }

    pub const fn is_sqlite(self) -> bool {
        matches!(self, Self::Sqlite)
    }
}

impl fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Error)]
#[error("invalid database type {value:?} (expected sqlite or mysql)")]
pub struct ParseDatabaseTypeError {
    pub value: String,
}

impl FromStr for DatabaseType {
    type Err = ParseDatabaseTypeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "sqlite" | "sqlite3" => Ok(Self::Sqlite),
            "mysql" => Ok(Self::Mysql),
            _ => Err(ParseDatabaseTypeError {
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub db_type: DatabaseType,
    /// SQLite database file path (`KOMARI_DB_FILE`).
    pub file: String,
    /// Host used by network databases (`KOMARI_DB_HOST`).
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub name: String,
}

impl DatabaseConfig {
    pub fn is_sqlite(&self) -> bool {
        self.db_type.is_sqlite()
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            db_type: DatabaseType::Sqlite,
            file: DEFAULT_DB_FILE.to_owned(),
            host: "localhost".to_owned(),
            port: 3306,
            user: "root".to_owned(),
            password: String::new(),
            name: "komari".to_owned(),
        }
    }
}

/// Fully resolved, strongly typed startup configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub listen: String,
    pub db: DatabaseConfig,
    pub log_level: LogLevel,
    pub theme: String,
    pub allow_cors: bool,
    pub enable_cloudflared: bool,
    pub cloudflared_token: Option<String>,
    pub ws_disable_origin: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: DEFAULT_LISTEN.to_owned(),
            db: DatabaseConfig::default(),
            log_level: LogLevel::Info,
            theme: "default".to_owned(),
            allow_cors: false,
            enable_cloudflared: false,
            cloudflared_token: None,
            ws_disable_origin: false,
        }
    }
}

/// Command-line values. Every field is optional: `None` means "fall through to
/// the environment variable and then the built-in default".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigOverrides {
    pub listen: Option<String>,
    pub db_type: Option<DatabaseType>,
    pub db_file: Option<String>,
    pub db_host: Option<String>,
    pub db_port: Option<u16>,
    pub db_user: Option<String>,
    pub db_password: Option<String>,
    pub db_name: Option<String>,
    pub log_level: Option<LogLevel>,
    pub theme: Option<String>,
    pub allow_cors: Option<bool>,
    pub enable_cloudflared: Option<bool>,
    pub ws_disable_origin: Option<bool>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("invalid value for {key}: {value:?} (expected {expected})")]
    InvalidValue {
        key: String,
        value: String,
        expected: &'static str,
    },
}

impl Config {
    /// Load configuration from the process environment and built-in defaults.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_with(ConfigOverrides::default(), LogLevel::Info)
    }

    /// Load configuration with command-line overrides. `default_log_level` is
    /// used only when neither a flag nor `KOMARI_LOG_LEVEL` is present; the CLI
    /// passes `debug` for development builds with an unknown version hash.
    pub fn load_with(
        overrides: ConfigOverrides,
        default_log_level: LogLevel,
    ) -> Result<Self, ConfigError> {
        Self::from_sources(|key| env::var(key).ok(), &overrides, default_log_level)
    }

    /// Resolve configuration from an injectable environment lookup. This keeps
    /// precedence testable without mutating process-global environment state.
    pub fn from_sources<F>(
        lookup: F,
        overrides: &ConfigOverrides,
        default_log_level: LogLevel,
    ) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        // The Go `GetEnv` helper treats an empty value exactly like an unset
        // variable, so empty environment values fall through to the default.
        let read = |key: &str| lookup(key).filter(|value| !value.is_empty());

        let listen = pick_string(overrides.listen.as_deref(), read(keys::ENV_LISTEN))
            .unwrap_or_else(|| DEFAULT_LISTEN.to_owned());

        let db_type = match overrides.db_type {
            Some(value) => value,
            None => match read(keys::ENV_DB_TYPE) {
                Some(value) => {
                    value
                        .parse::<DatabaseType>()
                        .map_err(|_| ConfigError::InvalidValue {
                            key: keys::ENV_DB_TYPE.to_owned(),
                            value,
                            expected: "sqlite or mysql",
                        })?
                }
                None => DatabaseType::default(),
            },
        };

        let db_file = pick_string(overrides.db_file.as_deref(), read(keys::ENV_DB_FILE))
            .unwrap_or_else(|| DEFAULT_DB_FILE.to_owned());
        let db_host = pick_string(overrides.db_host.as_deref(), read(keys::ENV_DB_HOST))
            .unwrap_or_else(|| "localhost".to_owned());
        let db_port = match overrides.db_port {
            Some(port) => port,
            None => match read(keys::ENV_DB_PORT) {
                Some(value) => parse_port(keys::ENV_DB_PORT, &value)?,
                None => 3306,
            },
        };
        let db_user = pick_string(overrides.db_user.as_deref(), read(keys::ENV_DB_USER))
            .unwrap_or_else(|| "root".to_owned());
        let db_password = pick_string(overrides.db_password.as_deref(), read(keys::ENV_DB_PASS))
            .unwrap_or_default();
        let db_name = pick_string(overrides.db_name.as_deref(), read(keys::ENV_DB_NAME))
            .unwrap_or_else(|| "komari".to_owned());

        let log_level = match overrides.log_level {
            Some(level) => level,
            None => match read(keys::ENV_LOG_LEVEL) {
                Some(value) => parse_log_level(keys::ENV_LOG_LEVEL, &value)?,
                None => default_log_level,
            },
        };

        let theme = pick_string(overrides.theme.as_deref(), read(keys::ENV_THEME))
            .unwrap_or_else(|| "default".to_owned());
        let allow_cors = match overrides.allow_cors {
            Some(value) => value,
            None => match read(keys::ENV_ALLOW_CORS) {
                Some(value) => parse_bool(keys::ENV_ALLOW_CORS, &value)?,
                None => false,
            },
        };
        let enable_cloudflared = match overrides.enable_cloudflared {
            Some(value) => value,
            None => match read(keys::ENV_ENABLE_CLOUDFLARED) {
                Some(value) => parse_bool(keys::ENV_ENABLE_CLOUDFLARED, &value)?,
                None => false,
            },
        };
        let cloudflared_token = read(keys::ENV_CLOUDFLARED_TOKEN);
        let ws_disable_origin = match overrides.ws_disable_origin {
            Some(value) => value,
            None => match read(keys::ENV_WS_DISABLE_ORIGIN) {
                Some(value) => parse_bool(keys::ENV_WS_DISABLE_ORIGIN, &value)?,
                None => false,
            },
        };

        Ok(Self {
            listen,
            db: DatabaseConfig {
                db_type,
                file: db_file,
                host: db_host,
                port: db_port,
                user: db_user,
                password: db_password,
                name: db_name,
            },
            log_level,
            theme,
            allow_cors,
            enable_cloudflared,
            cloudflared_token,
            ws_disable_origin,
        })
    }
}

fn pick_string(flag: Option<&str>, env: Option<String>) -> Option<String> {
    flag.map(str::to_owned).or(env)
}

fn parse_port(key: &str, value: &str) -> Result<u16, ConfigError> {
    value.parse::<u16>().map_err(|_| ConfigError::InvalidValue {
        key: key.to_owned(),
        value: value.to_owned(),
        expected: "an integer between 0 and 65535",
    })
}

fn parse_log_level(key: &str, value: &str) -> Result<LogLevel, ConfigError> {
    value
        .parse::<LogLevel>()
        .map_err(|_| ConfigError::InvalidValue {
            key: key.to_owned(),
            value: value.to_owned(),
            expected: "trace, debug, info, warn, error, or off",
        })
}

fn parse_bool(key: &str, value: &str) -> Result<bool, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::InvalidValue {
            key: key.to_owned(),
            value: value.to_owned(),
            expected: "true or false",
        }),
    }
}
