use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::FromRow;

/// Theme metadata (`komari-theme.json`). This is not a database table; it is
/// included because the Go `models.Theme` type participates in the model set.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub name: String,
    pub short: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub url: String,
    pub preview: String,
    pub configuration: Configuration,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Configuration {
    #[serde(rename = "type", default)]
    pub configuration_type: String,
    pub icon: String,
    pub name: Value,
    pub data: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ManagedThemeConfigurationItem {
    pub key: String,
    pub name: Value,
    pub required: bool,
    #[serde(rename = "type")]
    pub item_type: String,
    pub options: String,
    pub default: Value,
    pub help: Value,
}

/// Stored per-theme configuration (`theme_configurations`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfiguration {
    pub short: String,
    /// JSON-encoded configuration object.
    pub data: String,
}

impl ThemeConfiguration {
    pub fn empty(short: impl Into<String>) -> Self {
        Self {
            short: short.into(),
            data: "{}".to_owned(),
        }
    }
}

impl FromRow for ThemeConfiguration {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            short: row.get("short")?,
            data: row.get("data")?,
        })
    }
}
