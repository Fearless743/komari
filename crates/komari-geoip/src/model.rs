//! Country information returned by GeoIP providers.

use serde::{Deserialize, Serialize};

/// Geographic information for one IP address.
///
/// `ISOCode` and `Name` mirror Go's `geoip.GeoInfo` exactly and are the only
/// fields serialized. The remaining fields retain extra provider details in
/// memory and are skipped by serde so API responses stay byte-compatible with
/// the Go backend (which has no such fields).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeoInfo {
    #[serde(rename = "ISOCode", alias = "iso_code")]
    pub iso_code: String,
    #[serde(rename = "Name", alias = "name")]
    pub name: String,
    #[serde(skip_serializing)]
    pub country: Option<String>,
    #[serde(skip_serializing)]
    pub province: Option<String>,
    #[serde(skip_serializing)]
    pub city: Option<String>,
    #[serde(skip_serializing)]
    pub isp: Option<String>,
    #[serde(skip_serializing)]
    pub latitude: Option<f64>,
    #[serde(skip_serializing)]
    pub longitude: Option<f64>,
}

impl GeoInfo {
    /// Build a country-only record, the shape every provider can supply.
    pub fn country(iso_code: impl Into<String>, name: impl Into<String>) -> Self {
        let iso_code = iso_code.into();
        let name = name.into();
        Self {
            iso_code,
            name,
            ..Self::default()
        }
    }

    /// Whether no country data is available.
    pub fn is_empty(&self) -> bool {
        self.iso_code.is_empty() && self.name.is_empty()
    }
}

/// Convert an ISO 3166-1 alpha-2 code to a regional-indicator emoji.
///
/// This is a direct port of `geoip.GetRegionUnicodeEmoji`: anything that is
/// not exactly two ASCII letters yields an empty string.
pub fn region_unicode_emoji(iso_code: &str) -> String {
    let bytes = iso_code.as_bytes();
    if bytes.len() != 2 || !bytes.iter().all(u8::is_ascii_alphabetic) {
        return String::new();
    }
    let iso_code = iso_code.to_ascii_uppercase();
    iso_code
        .chars()
        .filter_map(|character| {
            let offset = character as u32 - 'A' as u32;
            char::from_u32(0x1F1E6 + offset)
        })
        .collect()
}
