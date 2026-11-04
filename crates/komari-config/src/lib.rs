//! Startup configuration for Komari.
//!
//! Configuration is resolved in the same order as the Go backend:
//!
//! 1. command-line flags (highest priority)
//! 2. environment variables
//! 3. built-in defaults (lowest priority)
//!
//! The environment variable names intentionally mirror the Go implementation
//! (`KOMARI_DB_TYPE`, `KOMARI_DB_FILE`, ...) so an existing deployment can be
//! migrated without changing its environment file.

pub mod config;
pub mod keys;
pub mod log_level;
pub mod site;

pub use config::{Config, ConfigError, ConfigOverrides, DatabaseConfig, DatabaseType};
pub use log_level::LogLevel;
pub use site::{DEFAULT_DESCRIPTION, DEFAULT_SITENAME, SiteConfig};
