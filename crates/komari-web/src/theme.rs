//! Parsing of `komari-theme.json` and theme metadata.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::PREVIEW_FILES;

/// Metadata from a theme's `komari-theme.json`.
///
/// The field names mirror the Go `models.Theme` type. Optional fields default
/// to empty values so older or hand-written themes keep working; `name` and
/// `short` are required by [`ThemeInfo::validate`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeInfo {
    #[serde(default)]
    pub name: String,
    /// Directory name / theme identifier, for example `default`.
    #[serde(default)]
    pub short: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub url: String,
    /// Preview image path relative to the theme root.
    #[serde(default)]
    pub preview: String,
    /// Managed theme configuration, kept opaque for the API layer.
    #[serde(default)]
    pub configuration: Value,
}

#[derive(Debug, Error)]
pub enum ThemeError {
    #[error("invalid theme JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("theme config is missing required field(s): {0}")]
    MissingFields(String),
}

impl ThemeInfo {
    pub fn from_slice(data: &[u8]) -> Result<Self, ThemeError> {
        let info: Self = serde_json::from_slice(data)?;
        info.validate()?;
        Ok(info)
    }

    pub fn from_json(json: &str) -> Result<Self, ThemeError> {
        Self::from_slice(json.as_bytes())
    }

    /// Reject configs that cannot identify a theme.
    ///
    /// This matches the validation Go performs before installing a theme
    /// package (`name` and `short` are mandatory).
    pub fn validate(&self) -> Result<(), ThemeError> {
        let mut missing = Vec::new();
        if self.name.trim().is_empty() {
            missing.push("name");
        }
        if self.short.trim().is_empty() {
            missing.push("short");
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(ThemeError::MissingFields(missing.join(", ")))
        }
    }

    /// Preview image candidates in priority order.
    ///
    /// Themes historically shipped `perview.png` (a typo that is still present
    /// in the Go repository), so both spellings are accepted.
    pub fn preview_candidates(&self) -> Vec<String> {
        let mut candidates = Vec::new();
        if !self.preview.trim().is_empty() {
            candidates.push(self.preview.trim().to_owned());
        }
        for name in PREVIEW_FILES {
            if !candidates.iter().any(|candidate| candidate == name) {
                candidates.push((*name).to_owned());
            }
        }
        candidates
    }
}
