mod support;

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use komari_config::keys::{GEO_IP_ENABLED_KEY, GEO_IP_PROVIDER_KEY};
use komari_db::Db;
use komari_geoip::error::GeoIpError;
use komari_geoip::factory::GeoIpFactory;
use komari_geoip::model::GeoInfo;
use komari_geoip::provider::GeoIpProvider;
use komari_geoip::providers::empty::EmptyProvider;
use komari_geoip::providers::geojs::GeoJsProvider;
use komari_geoip::providers::ipapi::IpApiProvider;
use komari_geoip::providers::ipinfo::IpInfoProvider;
use komari_geoip::providers::mmdb::MaxMindProvider;
use komari_geoip::region_unicode_emoji;
use komari_geoip::service::GeoIpService;
use support::{MockResponse, MockServer};

const MMDB_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/GeoIP2-Country-Test.mmdb"
);

fn ip(value: &str) -> IpAddr {
    value.parse().expect("valid IP")
}

fn temp_mmdb_path() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "komari-geoip-test-{}-{nanos}.mmdb",
        std::process::id()
    ))
}

fn database() -> Arc<Db> {
    let db = Arc::new(Db::open_in_memory().expect("in-memory database"));
    db.migrate().expect("migrate");
    db
}

#[tokio::test]
async fn mmdb_lookup_maps_country_fields() {
    let provider = MaxMindProvider::open(MMDB_FIXTURE).expect("open fixture database");
    assert_eq!(provider.name(), "MaxMind");

    let info = provider
        .get_geo_info(ip("89.160.20.112"))
        .await
        .expect("lookup");
    assert_eq!(info.iso_code, "SE");
    assert_eq!(info.name, "Sweden");
    assert_eq!(info.country.as_deref(), Some("Sweden"));
    assert!(!info.is_empty());
}

#[tokio::test]
async fn mmdb_unknown_address_returns_empty_record() {
    let provider = MaxMindProvider::open(MMDB_FIXTURE).expect("open fixture database");
    let info = provider
        .get_geo_info(ip("10.0.0.1"))
        .await
        .expect("lookup without data");
    assert!(info.is_empty());
    assert_eq!(info, GeoInfo::default());
}

#[tokio::test]
async fn mmdb_downloads_missing_database_and_reloads() {
    let database_bytes = std::fs::read(MMDB_FIXTURE).expect("fixture");
    let server = MockServer::start(move |_| {
        MockResponse::bytes(200, database_bytes.clone(), "application/octet-stream")
    })
    .await;
    let path = temp_mmdb_path();
    let _ = std::fs::remove_file(&path);

    let provider = MaxMindProvider::with_download_url(&path, server.url());
    assert!(!provider.is_loaded());
    assert!(!path.exists());

    provider.update_database().await.expect("download database");
    assert!(provider.is_loaded());
    assert!(path.exists());
    assert_eq!(server.request_count(), 1);

    let info = provider
        .get_geo_info(ip("89.160.20.112"))
        .await
        .expect("downloaded lookup");
    assert_eq!(info.iso_code, "SE");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn mmdb_existing_database_is_loaded_without_download() {
    let path = temp_mmdb_path();
    std::fs::copy(MMDB_FIXTURE, &path).expect("copy fixture");
    let server = MockServer::start(|_| MockResponse::text(500, "must not be called")).await;

    let provider = MaxMindProvider::with_download_url(&path, server.url());
    provider.update_database().await.expect("load existing");
    assert_eq!(server.request_count(), 0);

    let info = provider
        .get_geo_info(ip("89.160.20.112"))
        .await
        .expect("lookup");
    assert_eq!(info.iso_code, "SE");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn service_initialize_downloads_configured_mmdb_provider() {
    let database_bytes = std::fs::read(MMDB_FIXTURE).expect("fixture");
    let server = MockServer::start(move |_| {
        MockResponse::bytes(200, database_bytes.clone(), "application/octet-stream")
    })
    .await;
    let path = temp_mmdb_path();
    let _ = std::fs::remove_file(&path);

    let db = database();
    db.config().set(GEO_IP_ENABLED_KEY, &true).expect("enable");
    db.config()
        .set(GEO_IP_PROVIDER_KEY, "mmdb")
        .expect("provider");

    let factory = GeoIpFactory::new();
    let constructor_path = path.clone();
    let constructor_url = server.url();
    factory.register_provider(
        "mmdb",
        Arc::new(move || {
            Ok(Box::new(MaxMindProvider::with_download_url(
                constructor_path.clone(),
                constructor_url.clone(),
            )) as Box<_>)
        }),
    );

    let service = GeoIpService::new()
        .with_factory(factory)
        .with_database(Arc::clone(&db));
    service.initialize().await.expect("initialize");

    assert_eq!(service.provider_key(), "mmdb");
    let info = service.lookup(ip("89.160.20.112")).await;
    assert_eq!(info.iso_code, "SE");
    assert!(path.exists());
    assert_eq!(server.request_count(), 1);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn mmdb_missing_database_is_a_graceful_error() {
    let error = match MaxMindProvider::open("/nonexistent/komari-geoip.mmdb") {
        Ok(_) => panic!("missing database must not open"),
        Err(error) => error,
    };
    assert!(matches!(error, GeoIpError::MaxMind(_) | GeoIpError::Io(_)));
}

#[test]
fn region_emoji_and_json_match_go() {
    assert_eq!(region_unicode_emoji("CN"), "🇨🇳");
    assert_eq!(region_unicode_emoji("us"), "🇺🇸");
    assert_eq!(region_unicode_emoji("USA"), "");
    assert_eq!(region_unicode_emoji("C1"), "");
    assert_eq!(region_unicode_emoji(""), "");

    let mut info = GeoInfo::country("US", "United States");
    info.country = Some("United States".to_owned());
    info.province = Some("California".to_owned());
    info.city = Some("Mountain View".to_owned());
    info.isp = Some("Google LLC".to_owned());
    info.latitude = Some(37.4);
    info.longitude = Some(-122.1);
    assert_eq!(
        serde_json::to_string(&info).expect("serialize"),
        r#"{"ISOCode":"US","Name":"United States"}"#
    );
}

#[tokio::test]
async fn ipapi_parses_full_response_and_builds_go_url() {
    let server = MockServer::start(|_| {
        MockResponse::json(
            200,
            r#"{"status":"success","country":"United States","countryCode":"US","regionName":"California","city":"Mountain View","isp":"Google LLC","lat":37.4,"lon":-122.1}"#,
        )
    })
    .await;

    let provider = IpApiProvider::with_base_url(server.url());
    let info = provider
        .get_geo_info(ip("8.8.8.8"))
        .await
        .expect("ip-api lookup");
    assert_eq!(info.iso_code, "US");
    assert_eq!(info.name, "United States");
    assert_eq!(info.country.as_deref(), Some("United States"));
    assert_eq!(info.province.as_deref(), Some("California"));
    assert_eq!(info.city.as_deref(), Some("Mountain View"));
    assert_eq!(info.isp.as_deref(), Some("Google LLC"));
    assert_eq!(info.latitude, Some(37.4));
    assert_eq!(info.longitude, Some(-122.1));

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/8.8.8.8");
    assert!(
        requests[0]
            .query
            .contains("fields=status,message,country,countryCode")
    );
}

#[tokio::test]
async fn ipapi_reports_application_errors_and_missing_fields() {
    let server = MockServer::start(|_| {
        MockResponse::json(200, r#"{"status":"fail","message":"private range"}"#)
    })
    .await;
    let provider = IpApiProvider::with_base_url(server.url());
    let error = provider
        .get_geo_info(ip("192.168.1.1"))
        .await
        .expect_err("private range");
    assert!(error.to_string().contains("private range"));

    let server = MockServer::start(|_| MockResponse::json(200, r#"{"status":"success"}"#)).await;
    let provider = IpApiProvider::with_base_url(server.url());
    let info = provider
        .get_geo_info(ip("8.8.8.8"))
        .await
        .expect("missing optional fields");
    assert!(info.is_empty());
    assert_eq!(info.latitude, None);
}

#[tokio::test]
async fn ipinfo_parses_country_region_and_location() {
    let server = MockServer::start(|_| {
        MockResponse::json(
            200,
            r#"{"ip":"8.8.8.8","city":"Mountain View","region":"California","country":"US","loc":"37.4,-122.1","org":"AS15169 Google LLC"}"#,
        )
    })
    .await;

    let provider = IpInfoProvider::with_base_url(server.url());
    let info = provider
        .get_geo_info(ip("8.8.8.8"))
        .await
        .expect("ipinfo lookup");
    assert_eq!(info.iso_code, "US");
    assert_eq!(info.name, "US");
    assert_eq!(info.city.as_deref(), Some("Mountain View"));
    assert_eq!(info.province.as_deref(), Some("California"));
    assert_eq!(info.isp.as_deref(), Some("AS15169 Google LLC"));
    assert_eq!(info.latitude, Some(37.4));
    assert_eq!(info.longitude, Some(-122.1));

    assert_eq!(server.requests()[0].path, "/8.8.8.8/json");
}

#[tokio::test]
async fn ipinfo_reports_status_errors_and_missing_fields() {
    let server = MockServer::start(|_| MockResponse::text(429, "rate limited")).await;
    let provider = IpInfoProvider::with_base_url(server.url());
    let error = provider
        .get_geo_info(ip("8.8.8.8"))
        .await
        .expect_err("rate limited");
    assert!(matches!(error, GeoIpError::HttpStatus { status: 429, .. }));

    let server = MockServer::start(|_| MockResponse::json(200, r#"{}"#)).await;
    let provider = IpInfoProvider::with_base_url(server.url());
    let info = provider
        .get_geo_info(ip("8.8.8.8"))
        .await
        .expect("missing fields");
    assert!(info.is_empty());
    assert_eq!(info.latitude, None);
}

#[tokio::test]
async fn geojs_parses_string_coordinates_and_rejects_empty_country() {
    let server = MockServer::start(|_| {
        MockResponse::json(
            200,
            r#"{"country":"United States","country_code":"US","region":"California","city":"Mountain View","organization_name":"Google LLC","latitude":"37.4","longitude":"-122.1"}"#,
        )
    })
    .await;
    let provider = GeoJsProvider::with_base_url(server.url());
    let info = provider
        .get_geo_info(ip("8.8.8.8"))
        .await
        .expect("geojs lookup");
    assert_eq!(info.iso_code, "US");
    assert_eq!(info.name, "United States");
    assert_eq!(info.province.as_deref(), Some("California"));
    assert_eq!(info.city.as_deref(), Some("Mountain View"));
    assert_eq!(info.isp.as_deref(), Some("Google LLC"));
    assert_eq!(info.latitude, Some(37.4));
    assert_eq!(info.longitude, Some(-122.1));
    assert_eq!(server.requests()[0].path, "/8.8.8.8.json");

    let server = MockServer::start(|_| {
        MockResponse::json(200, r#"{"country":"Nowhere","country_code":""}"#)
    })
    .await;
    let provider = GeoJsProvider::with_base_url(server.url());
    let error = provider
        .get_geo_info(ip("192.168.1.1"))
        .await
        .expect_err("private address");
    assert!(error.to_string().contains("empty geo info"));
}

#[tokio::test]
async fn geojs_reports_status_errors() {
    let server = MockServer::start(|_| MockResponse::text(500, "unavailable")).await;
    let provider = GeoJsProvider::with_base_url(server.url());
    let error = provider
        .get_geo_info(ip("8.8.8.8"))
        .await
        .expect_err("geojs failure");
    assert!(matches!(error, GeoIpError::HttpStatus { status: 500, .. }));
    assert!(error.to_string().contains("unavailable"));
}

#[test]
fn factory_exposes_builtin_providers() {
    let factory = GeoIpFactory::with_builtins();
    assert_eq!(
        factory.provider_keys(),
        vec!["empty", "geojs", "ip-api", "ipinfo", "mmdb"]
    );
    assert!(factory.contains("ipinfo"));
    assert!(factory.create("ipinfo").is_ok());
    assert!(factory.create("geojs").is_ok());
    assert!(factory.create("ip-api").is_ok());
    assert!(factory.create("empty").is_ok());
    assert!(matches!(
        factory.create("unknown"),
        Err(GeoIpError::ProviderNotFound(name)) if name == "unknown"
    ));
}

#[tokio::test]
async fn service_without_provider_degrades_to_empty() {
    let service = GeoIpService::new();
    assert_eq!(service.lookup(ip("8.8.8.8")).await, GeoInfo::default());
    assert_eq!(service.lookup_str("not-an-ip").await, GeoInfo::default());
    assert!(matches!(
        service.try_lookup(ip("8.8.8.8")).await,
        Err(GeoIpError::EmptyProvider)
    ));
    assert!(matches!(
        service.try_lookup_str("not-an-ip").await,
        Err(GeoIpError::InvalidIp(_))
    ));
    assert!(service.update_database().await.is_err());
    assert!(service.close().await.is_ok());
}

#[tokio::test]
async fn disabled_geoip_uses_empty_provider() {
    let db = database();
    db.config()
        .set(GEO_IP_ENABLED_KEY, &false)
        .expect("disable geoip");
    let service = GeoIpService::new().with_database(Arc::clone(&db));
    service.initialize().await.expect("initialize");

    assert_eq!(service.provider_key(), "empty");
    assert_eq!(service.provider_name(), None);
    assert_eq!(service.lookup(ip("8.8.8.8")).await, GeoInfo::default());
}

#[tokio::test]
async fn unknown_provider_falls_back_to_empty() {
    let db = database();
    db.config().set(GEO_IP_ENABLED_KEY, &true).expect("enable");
    db.config()
        .set(GEO_IP_PROVIDER_KEY, "does-not-exist")
        .expect("provider");
    let service = GeoIpService::new().with_database(Arc::clone(&db));
    service.initialize().await.expect("initialize");

    assert_eq!(service.provider_key(), "empty");
    assert_eq!(service.lookup(ip("8.8.8.8")).await, GeoInfo::default());
}

#[tokio::test]
async fn initialize_defaults_to_ipinfo() {
    let db = database();
    let service = GeoIpService::new().with_database(Arc::clone(&db));
    service.initialize().await.expect("initialize");

    assert_eq!(service.provider_key(), "ipinfo");
    assert_eq!(service.provider_name().as_deref(), Some("ipinfo.io"));
    assert_eq!(
        db.config()
            .get_as::<String>(GEO_IP_PROVIDER_KEY)
            .unwrap()
            .unwrap(),
        "ipinfo"
    );
}

#[derive(Default)]
struct CountingProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl GeoIpProvider for CountingProvider {
    fn name(&self) -> &str {
        "counting"
    }

    async fn get_geo_info(&self, _ip: IpAddr) -> Result<GeoInfo, GeoIpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(GeoInfo::country("US", "United States"))
    }
}

#[tokio::test]
async fn service_caches_lookups_and_flushes_on_database_update() {
    let db = database();
    db.config().set(GEO_IP_ENABLED_KEY, &true).expect("enable");
    db.config()
        .set(GEO_IP_PROVIDER_KEY, "counting")
        .expect("provider");

    let calls = Arc::new(AtomicUsize::new(0));
    let factory = GeoIpFactory::new();
    let constructor_calls = Arc::clone(&calls);
    factory.register_provider(
        "counting",
        Arc::new(move || {
            Ok(Box::new(CountingProvider {
                calls: Arc::clone(&constructor_calls),
            }) as Box<_>)
        }),
    );

    let service = GeoIpService::new()
        .with_factory(factory)
        .with_database(Arc::clone(&db));
    service.initialize().await.expect("initialize");
    assert_eq!(service.provider_key(), "counting");

    let first = service.lookup(ip("8.8.8.8")).await;
    let second = service.lookup(ip("8.8.8.8")).await;
    assert_eq!(first, second);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(service.cache_len(), 1);

    service.update_database().await.expect("update database");
    assert_eq!(service.cache_len(), 0);
    let _ = service.lookup(ip("8.8.8.8")).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cache_expires_and_reinvokes_provider() {
    let db = database();
    db.config().set(GEO_IP_ENABLED_KEY, &true).expect("enable");
    db.config()
        .set(GEO_IP_PROVIDER_KEY, "counting")
        .expect("provider");

    let calls = Arc::new(AtomicUsize::new(0));
    let factory = GeoIpFactory::new();
    let constructor_calls = Arc::clone(&calls);
    factory.register_provider(
        "counting",
        Arc::new(move || {
            Ok(Box::new(CountingProvider {
                calls: Arc::clone(&constructor_calls),
            }) as Box<_>)
        }),
    );

    let service = GeoIpService::new()
        .with_factory(factory)
        .with_database(Arc::clone(&db))
        .with_cache_ttl(Duration::from_millis(10));
    service.initialize().await.expect("initialize");

    let _ = service.lookup(ip("8.8.8.8")).await;
    assert_eq!(service.cache_len(), 1);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(service.cache_len(), 0);

    let _ = service.lookup(ip("8.8.8.8")).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cache_capacity_evicts_old_entries() {
    let db = database();
    db.config().set(GEO_IP_ENABLED_KEY, &true).expect("enable");
    db.config()
        .set(GEO_IP_PROVIDER_KEY, "counting")
        .expect("provider");

    let calls = Arc::new(AtomicUsize::new(0));
    let factory = GeoIpFactory::new();
    let constructor_calls = Arc::clone(&calls);
    factory.register_provider(
        "counting",
        Arc::new(move || {
            Ok(Box::new(CountingProvider {
                calls: Arc::clone(&constructor_calls),
            }) as Box<_>)
        }),
    );

    let service = GeoIpService::new()
        .with_factory(factory)
        .with_database(Arc::clone(&db))
        .with_cache_capacity(2);
    service.initialize().await.expect("initialize");

    for address in ["8.8.8.8", "1.1.1.1", "9.9.9.9"] {
        let _ = service.lookup(ip(address)).await;
    }
    assert_eq!(service.cache_len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let _ = service.lookup(ip("8.8.8.8")).await;
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn empty_provider_reports_no_backend() {
    let provider = EmptyProvider;
    assert_eq!(provider.name(), "EmptyProvider");
    assert!(matches!(
        provider.get_geo_info(ip("8.8.8.8")).await,
        Err(GeoIpError::EmptyProvider)
    ));
    assert!(provider.update_database().await.is_err());
    assert!(provider.close().await.is_ok());
}
