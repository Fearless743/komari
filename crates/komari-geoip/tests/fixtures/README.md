# Test fixture

`GeoIP2-Country-Test.mmdb` comes from the
[MaxMind-DB](https://github.com/maxmind/MaxMind-DB) test-data set
(`test-data/GeoIP2-Country-Test.mmdb`). The MaxMind-DB repository is dual
licensed under Apache-2.0 and MIT; the test data is intended for testing
MaxMind DB readers.

It is used only by `tests/geoip_providers.rs` to verify that the `mmdb`
provider opens a real database, maps `country.iso_code` / `country.names.en`,
and returns an empty record for addresses outside the database.
