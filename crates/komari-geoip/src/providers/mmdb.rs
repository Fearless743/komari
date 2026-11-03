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
pub struct MaxMindProvider {
    path: PathBuf,
    download_url: String,
    reader: RwLock<Reader<Vec<u8>>>,
    client: reqwest::Client,
}

impl MaxMindProvider {
    /// Open the default database path.
    pub fn new() -> Result<Self, GeoIpError> {
        Self::open(GEOIP_FILE_PATH)
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
        let path = path.as_ref().to_path_buf();
        let reader = Reader::open_readfile(&path)?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("building the GeoIP HTTP client cannot fail");
        Ok(Self {
            path,
            download_url: download_url.into(),
            reader: RwLock::new(reader),
            client,
        })
    }

    /// Path of the loaded database.
    pub fn path(&self) -> &Path {
        &self.path
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
        *self.reader.write().expect("maxmind reader poisoned") = reader;
        Ok(())
    }
}

#[async_trait]
impl GeoIpProvider for MaxMindProvider {
    fn name(&self) -> &str {
        "MaxMind"
    }

    async fn get_geo_info(&self, ip: IpAddr) -> Result<GeoInfo, GeoIpError> {
        let reader = self.reader.read().expect("maxmind reader poisoned");
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

    async fn update_database(&self) -> Result<(), GeoIpError> {
        self.download().await
    }
}

/// Register the `mmdb` provider.
pub fn register(factory: &GeoIpFactory) {
    factory.register_provider(
        "mmdb",
        std::sync::Arc::new(|| MaxMindProvider::new().map(|provider| Box::new(provider) as Box<_>)),
    );
}
