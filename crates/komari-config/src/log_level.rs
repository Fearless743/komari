use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Log verbosity accepted by `KOMARI_LOG_LEVEL`, `--log-level`, and the
/// persisted configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Off,
}

impl LogLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Off => "off",
        }
    }

    /// Filter directive understood by `tracing-subscriber`.
    pub const fn as_filter(self) -> &'static str {
        self.as_str()
    }
}

impl Default for LogLevel {
    fn default() -> Self {
        Self::Info
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Error)]
#[error("invalid log level {value:?} (expected trace, debug, info, warn, error, off)")]
pub struct ParseLogLevelError {
    pub value: String,
}

impl FromStr for LogLevel {
    type Err = ParseLogLevelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            "off" | "none" => Ok(Self::Off),
            _ => Err(ParseLogLevelError {
                value: value.to_owned(),
            }),
        }
    }
}
