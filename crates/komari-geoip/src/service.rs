//! Provider selection, caching and graceful GeoIP lookups.
//!
//! Mirrors `geoip.InitGeoIp` / `geoip.GetGeoInfo`: the provider is selected by
//! the `geo_ip_provider` setting, lookups are cached for 48 hours, and a
//! missing or failing provider degrades to an empty result instead of
//! panicking.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use komari_config::keys::{GEO_IP_ENABLED_KEY, GEO_IP_PROVIDER_KEY};
use komari_db::Db;
use parking_lot::{Mutex, RwLock};

use crate::error::GeoIpError;
use crate::factory::GeoIpFactory;
use crate::model::GeoInfo;
use crate::provider::GeoIpProvider;

/// Default cache lifetime, matching Go's `48 * time.Hour`.
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(48 * 60 * 60);

/// Upper bound on cached lookups so long-running instances stay bounded.
pub const DEFAULT_CACHE_CAPACITY: usize = 10_000;

#[derive(Clone)]
struct CacheEntry {
    stored_at: Instant,
    info: GeoInfo,
}

/// Coordinates a GeoIP provider and its cache.
pub struct GeoIpService {
    db: Option<Arc<Db>>,
    factory: Arc<GeoIpFactory>,
    provider: RwLock<Option<Arc<dyn GeoIpProvider>>>,
    provider_key: RwLock<String>,
    cache: Mutex<HashMap<String, CacheEntry>>,
    cache_ttl: Duration,
    cache_capacity: usize,
}

impl Default for GeoIpService {
    fn default() -> Self {
        Self::new()
    }
}

impl GeoIpService {
    /// Create a service with no database and no active provider.
    pub fn new() -> Self {
        Self {
            db: None,
            factory: Arc::new(GeoIpFactory::with_builtins()),
            provider: RwLock::new(None),
            provider_key: RwLock::new("empty".to_owned()),
            cache: Mutex::new(HashMap::new()),
            cache_ttl: DEFAULT_CACHE_TTL,
            cache_capacity: DEFAULT_CACHE_CAPACITY,
        }
    }

    /// Attach the configuration database used by [`GeoIpService::initialize`].
    pub fn with_database(mut self, db: Arc<Db>) -> Self {
        self.db = Some(db);
        self
    }

    /// Replace the provider registry (used by tests).
    pub fn with_factory(mut self, factory: GeoIpFactory) -> Self {
        self.factory = Arc::new(factory);
        self.provider = RwLock::new(None);
        self
    }

    /// Override the cache lifetime.
    pub fn with_cache_ttl(mut self, ttl: Duration) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// Override the maximum number of cached lookups. Zero disables caching.
    pub fn with_cache_capacity(mut self, capacity: usize) -> Self {
        self.cache_capacity = capacity;
        self
    }

    pub fn db(&self) -> Option<&Db> {
        self.db.as_deref()
    }

    pub fn factory(&self) -> &GeoIpFactory {
        &self.factory
    }

    /// Configuration key of the active provider (`empty` when none).
    pub fn provider_key(&self) -> String {
        self.provider_key.read().clone()
    }

    /// Display name of the active provider.
    pub fn provider_name(&self) -> Option<String> {
        self.provider
            .read()
            .as_ref()
            .map(|provider| provider.name().to_owned())
    }

    /// Install a provider directly.
    pub fn set_provider(&self, key: &str, provider: Box<dyn GeoIpProvider>) {
        *self.provider.write() = Some(Arc::from(provider));
        *self.provider_key.write() = key.to_owned();
    }

    /// Port of `geoip.InitGeoIp`. Configuration failures leave the service on
    /// the empty provider and never propagate as a panic. Local providers
    /// (currently `mmdb`) are prepared here via [`GeoIpProvider::prepare`],
    /// which loads an existing database or downloads a missing one.
    pub async fn initialize(&self) -> Result<(), GeoIpError> {
        let Some(db) = self.db.as_ref() else {
            return Ok(());
        };
        let enabled: bool = db.config().get_as_or(GEO_IP_ENABLED_KEY, true)?;
        if !enabled {
            self.clear_provider();
            return Ok(());
        }

        let provider_key: String = db
            .config()
            .get_as_or(GEO_IP_PROVIDER_KEY, "ipinfo".to_owned())?;
        if provider_key.is_empty() {
            self.clear_provider();
            return Ok(());
        }

        match self.factory.create(&provider_key) {
            Ok(provider) => {
                if let Err(error) = provider.prepare().await {
                    tracing::error!("failed to initialize GeoIP provider {provider_key}: {error}");
                    self.clear_provider();
                    return Ok(());
                }
                *self.provider.write() = Some(Arc::from(provider));
                *self.provider_key.write() = provider_key.clone();
                self.cache.lock().clear();
                tracing::info!("using GeoIP provider {provider_key}");
            }
            Err(error) => {
                tracing::error!("failed to initialize GeoIP provider {provider_key}: {error}");
                self.clear_provider();
            }
        }
        Ok(())
    }

    /// Look up an address, returning an empty record if no provider is active.
    pub async fn lookup(&self, ip: IpAddr) -> GeoInfo {
        self.try_lookup(ip).await.unwrap_or_default()
    }

    /// Look up an address from text, degrading invalid input to an empty
    /// record.
    pub async fn lookup_str(&self, ip: &str) -> GeoInfo {
        match ip.trim().parse::<IpAddr>() {
            Ok(ip) => self.lookup(ip).await,
            Err(_) => GeoInfo::default(),
        }
    }

    /// Look up an address and surface provider errors.
    pub async fn try_lookup(&self, ip: IpAddr) -> Result<GeoInfo, GeoIpError> {
        let Some(provider) = self.provider.read().clone() else {
            return Err(GeoIpError::EmptyProvider);
        };
        let cache_key = format!("{}:{ip}", provider.name());
        if let Some(info) = self.cached(&cache_key) {
            return Ok(info);
        }

        let info = provider.get_geo_info(ip).await?;
        self.store(cache_key, info.clone());
        Ok(info)
    }

    /// Look up an address from text and surface parse/provider errors.
    pub async fn try_lookup_str(&self, ip: &str) -> Result<GeoInfo, GeoIpError> {
        let ip = ip
            .trim()
            .parse::<IpAddr>()
            .map_err(|_| GeoIpError::InvalidIp(ip.to_owned()))?;
        self.try_lookup(ip).await
    }

    /// Port of `geoip.UpdateDatabase`; clears the cache on success.
    pub async fn update_database(&self) -> Result<(), GeoIpError> {
        let provider = self
            .provider
            .read()
            .clone()
            .ok_or(GeoIpError::EmptyProvider)?;
        provider.update_database().await?;
        self.cache.lock().clear();
        tracing::info!("GeoIP cache cleared due to database update");
        Ok(())
    }

    /// Release provider resources.
    pub async fn close(&self) -> Result<(), GeoIpError> {
        let provider = self.provider.read().clone();
        if let Some(provider) = provider {
            provider.close().await?;
        }
        self.cache.lock().clear();
        Ok(())
    }

    /// Number of live cached lookups. Expired entries are evicted while
    /// counting.
    pub fn cache_len(&self) -> usize {
        let mut cache = self.cache.lock();
        if self.cache_ttl.is_zero() {
            cache.clear();
            return 0;
        }
        cache.retain(|_, entry| entry.stored_at.elapsed() <= self.cache_ttl);
        cache.len()
    }

    fn cached(&self, key: &str) -> Option<GeoInfo> {
        if self.cache_ttl.is_zero() {
            return None;
        }
        let mut cache = self.cache.lock();
        if let Some(entry) = cache.get(key) {
            if entry.stored_at.elapsed() <= self.cache_ttl {
                return Some(entry.info.clone());
            }
        }
        cache.remove(key);
        None
    }

    fn store(&self, key: String, info: GeoInfo) {
        if self.cache_capacity == 0 || self.cache_ttl.is_zero() {
            return;
        }
        let mut cache = self.cache.lock();
        if cache.len() >= self.cache_capacity {
            cache.retain(|_, entry| entry.stored_at.elapsed() <= self.cache_ttl);
        }
        while cache.len() >= self.cache_capacity {
            let oldest = cache
                .iter()
                .min_by_key(|(_, entry)| entry.stored_at)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else {
                break;
            };
            cache.remove(&oldest);
        }
        cache.insert(
            key,
            CacheEntry {
                stored_at: Instant::now(),
                info,
            },
        );
    }

    fn clear_provider(&self) {
        *self.provider.write() = None;
        *self.provider_key.write() = "empty".to_owned();
        self.cache.lock().clear();
    }
}
