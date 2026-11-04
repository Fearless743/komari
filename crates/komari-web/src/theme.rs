//! Parsing of `komari-theme.json` and theme metadata.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{DEFAULT_THEME, PREVIEW_FILES};

/// Managed theme configuration (`models.Configuration` in Go).
///
/// The shape is part of the API contract: a theme without a `configuration`
/// object still serializes as
/// `{"type":"","icon":"","name":null,"data":null}`, and a non-object value is
/// rejected while parsing. `name` and `data` are opaque JSON.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeConfiguration {
    #[serde(rename = "type", default, deserialize_with = "null_to_default")]
    pub configuration_type: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub icon: String,
    #[serde(default)]
    pub name: Value,
    #[serde(default)]
    pub data: Value,
}

/// Metadata from a theme's `komari-theme.json`.
///
/// The field names and JSON shape mirror the Go `models.Theme` type. Optional
/// fields default to empty values so older or hand-written themes keep working;
/// `name` and `short` are required by [`ThemeInfo::validate`]. A JSON `null` in
/// any string field is treated as the zero value, exactly like Go's
/// `encoding/json` when decoding into a struct.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeInfo {
    #[serde(default, deserialize_with = "null_to_default")]
    pub name: String,
    /// Directory name / theme identifier, for example `default`.
    #[serde(default, deserialize_with = "null_to_default")]
    pub short: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub description: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub version: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub author: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub url: String,
    /// Preview image path relative to the theme root.
    #[serde(default, deserialize_with = "null_to_default")]
    pub preview: String,
    /// Managed theme configuration. Missing or `null` becomes the zero-value
    /// object; any other non-object value is a parse error.
    #[serde(default, deserialize_with = "deserialize_configuration")]
    pub configuration: ThemeConfiguration,
}

/// Deserialize `T`, treating JSON `null` as `T::default()`.
///
/// Go decodes `null` into string and struct fields without error, leaving the
/// zero value in place. Serde would otherwise reject it for `String` and for
/// structs.
fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Deserialize a configuration object, accepting `null` as the zero value.
///
/// A plain `Option<ThemeConfiguration>` would also accept a JSON array, because
/// serde can decode structs from sequences; Go only accepts an object (or
/// `null`) for `models.Configuration`.
fn deserialize_configuration<'de, D>(deserializer: D) -> Result<ThemeConfiguration, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<Value>::deserialize(deserializer)? {
        None => Ok(ThemeConfiguration::default()),
        Some(Value::Object(map)) => {
            serde_json::from_value(Value::Object(map)).map_err(serde::de::Error::custom)
        }
        Some(_) => Err(serde::de::Error::custom(
            "configuration must be an object or null",
        )),
    }
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
    /// package: `name` and `short` must not be the empty string. Whitespace is
    /// *not* trimmed, so a whitespace-only name passes, exactly like Go.
    pub fn validate(&self) -> Result<(), ThemeError> {
        let mut missing = Vec::new();
        if self.name.is_empty() {
            missing.push("name");
        }
        if self.short.is_empty() {
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

/// Validate a theme package's `short` field the way Go's `isValidThemeShort`
/// does before installing it.
///
/// `default` is reserved for the embedded theme; only ASCII letters, digits,
/// `_` and `-` are accepted. This is stricter than [`crate::is_safe_theme_id`],
/// which merely guards path traversal for themes that are already installed.
pub fn is_valid_theme_short(short: &str) -> bool {
    !short.is_empty()
        && short != DEFAULT_THEME
        && short
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn installable_short_matches_go() {
        for valid in ["alpha", "Alpha_2", "my-theme", "0"] {
            assert!(is_valid_theme_short(valid), "{valid}");
        }
        for invalid in ["", "default", "a b", "a/b", "..", "tëma", "a.b", "a\\b"] {
            assert!(!is_valid_theme_short(invalid), "{invalid}");
        }
    }

    #[test]
    fn configuration_serializes_like_go() {
        let info = ThemeInfo::from_json(r#"{"name":"T","short":"t"}"#).unwrap();
        assert_eq!(info.configuration, ThemeConfiguration::default());
        assert_eq!(
            serde_json::to_value(&info).unwrap()["configuration"],
            json!({"type": "", "icon": "", "name": null, "data": null})
        );

        let info = ThemeInfo::from_json(
            r#"{"name":"T","short":"t","configuration":{"type":"managed","data":[]}}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&info).unwrap()["configuration"],
            json!({"type": "managed", "icon": "", "name": null, "data": []})
        );

        let info =
            ThemeInfo::from_json(r#"{"name":"T","short":"t","configuration":null}"#).unwrap();
        assert_eq!(info.configuration, ThemeConfiguration::default());
    }

    #[test]
    fn configuration_must_be_an_object_or_null() {
        for config in [r#""managed""#, "[]", "1", "true"] {
            let json = format!(r#"{{"name":"T","short":"t","configuration":{config}}}"#);
            let error = ThemeInfo::from_json(&json).expect_err("non-object configuration");
            assert!(matches!(error, ThemeError::Json(_)), "{config}: {error}");
        }

        // A wrong-typed `type` field is rejected like Go's struct decoder.
        let error = ThemeInfo::from_json(r#"{"name":"T","short":"t","configuration":{"type":1}}"#)
            .expect_err("non-string type");
        assert!(matches!(error, ThemeError::Json(_)));
    }

    #[test]
    fn null_string_fields_are_zero_values() {
        let info = ThemeInfo::from_json(
            r#"{"name":"T","short":"t","description":null,"version":null,"author":null,"url":null,"preview":null}"#,
        )
        .unwrap();
        assert_eq!(info.description, "");
        assert_eq!(info.version, "");
        assert_eq!(info.author, "");
        assert_eq!(info.url, "");
        assert_eq!(info.preview, "");

        let error = ThemeInfo::from_json(r#"{"name":null,"short":"t"}"#)
            .expect_err("null name is the empty string");
        assert!(error.to_string().contains("name"));
    }

    #[test]
    fn whitespace_name_is_accepted_like_go() {
        let info = ThemeInfo::from_json(r#"{"name":" ","short":"t"}"#).unwrap();
        assert_eq!(info.name, " ");
        assert_eq!(info.short, "t");
    }
}
