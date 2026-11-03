# Test fixture provenance

`GeoIP2-Country-Test.mmdb` comes from the MaxMind-DB test-data set:

- Repository: <https://github.com/maxmind/MaxMind-DB>
- Upstream commit: `d1551916cff0712d12ff602c309234471b0402a2` (main, 2026-10-26)
- Path: `test-data/GeoIP2-Country-Test.mmdb`
- SHA-256: `fc12d6470b35b70bd3e4534dd433d3e262a6c73f7682f85c344e832d4190f02b`
- Local copy added: 2026-11-04

MaxMind-DB is dual licensed under Apache-2.0 and MIT; the upstream
`LICENSE-APACHE` and `LICENSE-MIT` files apply to the repository contents,
including this test database.

The fixture is used only by `tests/geoip_providers.rs` to verify that the
`mmdb` provider opens a real database, maps `country.iso_code` /
`country.names.en`, and returns an empty record for addresses outside the
database. It is also served by the local mock HTTP server to exercise the
"missing database is downloaded and reloaded" path without network access.
