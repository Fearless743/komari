//! MaxMind `.mmdb` GeoIP provider.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use async_trait::async_trait;
use maxminddb::Reader;
use maxminddb::geoip2;

use crate::error::GeoIpError;
use crate::factory::GeoIpFactory;
use crate::model::GeoInfo;
use crate::provider::GeoIpProvider;

/// Default download URL, matching Go's `GeoIpUrl`.
pub const GEOIP_URL: &str =
    "https://raw.githubusercontent.com/Loyalsoldier/geoip/release/GeoLite2-Country.mmdb";

/// Default database path, matching Go's `GeoIpFilePath`.
pub const GEOIP_FILE_PATH: &str = "./data/GeoLite2-Country.mmdb";

/// MaxMind database provider.
///
/// Construction is lazy: the reader is loaded on the first
/// [`GeoIpProvider::update_database`] call (or by [`MaxMindProvider::open`]).
/// This mirrors Go's `NewMaxMindGeoIPService`, which creates the data
/// directory, downloads a missing database and then opens it.
pub struct MaxMindProvider {
    path: PathBuf,
    download_url: String,
    reader: RwLock<Option<Reader<Vec<u8>>>>,
    client: reqwest::Client,
}

impl MaxMindProvider {
    /// Create a provider for the default database path without touching disk.
    /// The parent directory is created eagerly, matching the Go constructor's
    /// `os.MkdirAll` call.
    pub fn new() -> Self {
        Self::with_download_url(GEOIP_FILE_PATH, GEOIP_URL)
    }

    /// Open an existing database file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GeoIpError> {
        Self::open_with_url(path, GEOIP_URL)
    }

    /// Open an existing database file with a custom download URL.
    pub fn open_with_url(
        path: impl AsRef<Path>,
        download_url: impl Into<String>,
    ) -> Result<Self, GeoIpError> {
        let provider = Self::with_download_url(path, download_url);
        provider.reload()?;
        Ok(provider)
    }

    /// Create a lazy provider for `path` using a custom download URL.
    pub fn with_download_url(path: impl AsRef<Path>, download_url: impl Into<String>) -> Self {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(error) = std::fs::create_dir_all(parent) {
                    tracing::warn!(
                        "failed to create MaxMind data directory {}: {error}",
                        parent.display()
                    );
                }
            }
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("building the GeoIP HTTP client cannot fail");
        Self {
            path,
            download_url: download_url.into(),
            reader: RwLock::new(None),
            client,
        }
    }

    /// Path of the loaded database.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether a database is currently loaded.
    pub fn is_loaded(&self) -> bool {
        self.reader
            .read()
            .expect("maxmind reader poisoned")
            .is_some()
    }

    /// Download the database to [`MaxMindProvider::path`] and reload it.
    pub async fn download(&self) -> Result<(), GeoIpError> {
        let response = self.client.get(&self.download_url).send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GeoIpError::HttpStatus {
                service: "MaxMind database",
                status: status.as_u16(),
                body,
            });
        }
        let bytes = response.bytes().await?;

        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(&self.path, &bytes)?;
        self.reload()
    }

    /// Re-open the database file, replacing the current reader.
    pub fn reload(&self) -> Result<(), GeoIpError> {
        let reader = Reader::open_readfile(&self.path)?;
        *self.reader.write().expect("maxmind reader poisoned") = Some(reader);
        Ok(())
    }
}

impl Default for MaxMindProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GeoIpProvider for MaxMindProvider {
    fn name(&self) -> &str {
        "MaxMind"
    }

    async fn get_geo_info(&self, ip: IpAddr) -> Result<GeoInfo, GeoIpError> {
        let reader = self.reader.read().expect("maxmind reader poisoned");
        let Some(reader) = reader.as_ref() else {
            return Err(GeoIpError::DatabaseUnavailable);
        };
        let result = reader.lookup(ip)?;
        if !result.has_data() {
            return Ok(GeoInfo::default());
        }
        let Some(record) = result.decode::<geoip2::Country>()? else {
            return Ok(GeoInfo::default());
        };

        let iso_code = record.country.iso_code.unwrap_or_default().to_owned();
        let name = record.country.names.english.unwrap_or_default().to_owned();
        let name = if name.is_empty() && !iso_code.is_empty() {
            iso_code.clone()
        } else {
            name
        };
        let country = if name.is_empty() {
            None
        } else {
            Some(name.clone())
        };
        Ok(GeoInfo {
            iso_code,
            name,
            country,
            ..GeoInfo::default()
        })
    }

    /// Go's initialization path: load an existing file, otherwise download it.
    /// An already loaded provider forces a fresh download, matching the admin
    /// "update GeoIP database" action.
    async fn update_database(&self) -> Result<(), GeoIpError> {
        if !self.is_loaded() && self.path.exists() {
            return self.reload();
        }
        self.download().await
    }
}

/// Register the `mmdb` provider.
pub fn register(factory: &GeoIpFactory) {
    factory.register_provider(
        "mmdb",
        std::sync::Arc::new(|| Ok(Box::new(MaxMindProvider::new()) as Box<_>)),
    );
}
