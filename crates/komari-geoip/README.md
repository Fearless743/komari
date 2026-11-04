# komari-geoip

Rust port of the Go backend's `utils/geoip` package.

| Go | Rust |
| --- | --- |
| `geoip.GeoIPService` | `provider::GeoIpProvider` |
| `EmptyProvider` | `providers::empty::EmptyProvider` |
| `MaxMindGeoIPService` | `providers::mmdb::MaxMindProvider` |
| `IPAPIService` / `IPInfoService` / `GeoJSService` | `providers::{ipapi,ipinfo,geojs}` |
| `InitGeoIp` / `GetGeoInfo` | `GeoIpService::{initialize,lookup}` |

## Caching

`GeoIpService` caches successful lookups for `DEFAULT_CACHE_TTL` (48 hours),
keyed by provider name and address, exactly like Go's
`cache.New(48*time.Hour, 1*time.Hour)` instance in `utils/geoip/geoip.go`.

Go's `go-cache` does not impose a capacity limit; entries expire by TTL only.
This crate follows the same policy: there is no entry-count cap and no
oldest-entry eviction. A background janitor (`spawn_janitor`, one-hour interval
by default) removes expired entries, and lookups/`cache_len()` also purge
lazily. `with_cache_ttl(Duration::ZERO)` disables caching.

## Providers

Built-in provider keys are `empty`, `mmdb`, `ip-api`, `ipinfo`, and `geojs`.
`geo_ip_enabled=false`, an unknown key, or a provider initialization failure
falls back to `EmptyProvider`, whose lookups return an empty `GeoInfo` rather
than panicking. `mmdb` loads `./data/GeoLite2-Country.mmdb` and downloads it on
first initialization when missing.
