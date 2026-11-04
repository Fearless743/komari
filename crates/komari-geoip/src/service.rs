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
use crate::providers::empty::EmptyProvider;

/// Default cache lifetime, matching Go's `48 * time.Hour`.
///
/// Go's `go-cache` instance in `utils/geoip/geoip.go` has no capacity limit:
/// entries only expire after this TTL, and the janitor's one-hour interval
/// controls when expired entries are reclaimed. The Rust cache follows the
/// same policy and grows without a hard entry cap.
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(48 * 60 * 60);

/// Default interval for the background cache janitor, matching Go's
/// `cache.New(48*time.Hour, 1*time.Hour)` cleanup interval.
pub const DEFAULT_JANITOR_INTERVAL: Duration = Duration::from_secs(60 * 60);

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
            provider: RwLock::new(Some(Arc::new(EmptyProvider))),
            provider_key: RwLock::new("empty".to_owned()),
            cache: Mutex::new(HashMap::new()),
            cache_ttl: DEFAULT_CACHE_TTL,
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

    /// Override the cache lifetime. A zero duration disables caching.
    pub fn with_cache_ttl(mut self, ttl: Duration) -> Self {
        self.cache_ttl = ttl;
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
        self.purge_expired();
        self.cache.lock().len()
    }

    /// Remove expired cache entries and return how many were dropped.
    ///
    /// Go's `go-cache` runs this from a janitor every hour; [`spawn_janitor`]
    /// provides the same background cleanup.
    pub fn purge_expired(&self) -> usize {
        let mut cache = self.cache.lock();
        if self.cache_ttl.is_zero() {
            let removed = cache.len();
            cache.clear();
            return removed;
        }
        let before = cache.len();
        cache.retain(|_, entry| entry.stored_at.elapsed() <= self.cache_ttl);
        before - cache.len()
    }

    /// Spawn the hourly cache janitor. The returned handle can abort the task;
    /// dropping the runtime also stops it.
    pub fn spawn_janitor(self: &Arc<Self>, interval: Duration) -> GeoIpJanitor {
        let service = Arc::clone(self);
        let interval = if interval.is_zero() {
            DEFAULT_JANITOR_INTERVAL
        } else {
            interval
        };
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first tick is immediate; skip it so the first sweep happens
            // after one interval, like go-cache.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let removed = service.purge_expired();
                if removed > 0 {
                    tracing::debug!(removed, "GeoIP janitor removed expired cache entries");
                }
            }
        });
        GeoIpJanitor { handle }
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
        if self.cache_ttl.is_zero() {
            return;
        }
        self.cache.lock().insert(
            key,
            CacheEntry {
                stored_at: Instant::now(),
                info,
            },
        );
    }

    fn clear_provider(&self) {
        // Go keeps `CurrentProvider` on EmptyProvider rather than leaving it
        // unset, so `Name()` is always available.
        *self.provider.write() = Some(Arc::new(EmptyProvider));
        *self.provider_key.write() = "empty".to_owned();
        self.cache.lock().clear();
    }
}

/// Handle for the background GeoIP cache janitor.
///
/// The task is aborted when this handle is dropped or [`shutdown`] is called.
///
/// [`shutdown`]: GeoIpJanitor::shutdown
pub struct GeoIpJanitor {
    handle: tokio::task::JoinHandle<()>,
}

impl GeoIpJanitor {
    /// Stop the janitor task.
    pub fn shutdown(&self) {
        self.handle.abort();
    }

    /// Whether the janitor task has stopped.
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}

impl Drop for GeoIpJanitor {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GeoInfo;

    fn service(ttl: Duration) -> Arc<GeoIpService> {
        Arc::new(GeoIpService::new().with_cache_ttl(ttl))
    }

    fn insert_expired(service: &GeoIpService, key: &str) {
        let stored_at = Instant::now()
            .checked_sub(service.cache_ttl + Duration::from_secs(1))
            .expect("instant supports subtraction");
        service.cache.lock().insert(
            key.to_owned(),
            CacheEntry {
                stored_at,
                info: GeoInfo::default(),
            },
        );
    }

    #[test]
    fn purge_expired_removes_only_expired_entries() {
        let service = service(Duration::from_millis(50));
        insert_expired(&service, "old:1.1.1.1");
        service.store("live:8.8.8.8".to_owned(), GeoInfo::default());

        assert_eq!(service.purge_expired(), 1);
        assert_eq!(service.cache.lock().len(), 1);
        assert_eq!(service.purge_expired(), 0);
    }

    #[test]
    fn zero_ttl_purge_clears_everything() {
        let service = service(Duration::ZERO);
        service.store("live:8.8.8.8".to_owned(), GeoInfo::default());
        // `store` is a no-op with a zero TTL, so insert directly to verify the
        // purge path.
        service.cache.lock().insert(
            "manual:8.8.8.8".to_owned(),
            CacheEntry {
                stored_at: Instant::now(),
                info: GeoInfo::default(),
            },
        );
        assert_eq!(service.purge_expired(), 1);
        assert!(service.cache.lock().is_empty());
    }

    #[tokio::test]
    async fn janitor_removes_expired_entries_in_the_background() {
        let service = service(Duration::from_millis(20));
        insert_expired(&service, "old:1.1.1.1");
        assert_eq!(service.cache.lock().len(), 1);

        let janitor = service.spawn_janitor(Duration::from_millis(10));
        for _ in 0..50 {
            if service.cache.lock().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            service.cache.lock().is_empty(),
            "janitor should purge expired entries without a lookup"
        );
        janitor.shutdown();
    }
}
