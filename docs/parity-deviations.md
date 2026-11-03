# Parity deviations

This file records deliberate differences between the Rust backend and the Go
reference implementation. Each entry explains the reason and the compatibility
impact.

## DDNS provider configuration redaction

**Status:** intentional security deviation.

The Go admin API returns `models.DdnsProvider` directly, including the raw
`addition` JSON blob. The Rust implementation keeps `ddns_providers.addition`
unchanged in SQLite and uses the full value internally, but public API responses
must remove sensitive fields before serialization.

Use `komari_ddns::redact_addition(provider_name, &addition)` for stored rows,
or `komari_ddns::redact_provider_configuration(&provider)` for a live provider.

| Provider | Fields removed from API responses | Fields kept |
| --- | --- | --- |
| `cloudflare` | `api_token`, `zone_id` | `record_id`, `record_type`, `ttl`, `proxied` |
| `webhook` | `url`, `headers`, `body` | `method`, `content_type`, `response_json_path` |
| `empty` | none | all fields |

Rules:

- Sensitive fields are removed, not masked.
- Invalid JSON, `null`, arrays and other non-object values produce `{}`.
- Unknown providers produce `{}` because their sensitive fields are unknown.
- Webhook URLs are always removed: Slack/Discord-style paths and query
  strings may carry tokens even without username/password credentials.
- The original `addition` string and the database row are not modified.
- `Debug` output for Cloudflare and Webhook configurations redacts secrets.
- Sensitive parsing failures use fixed error messages; HTTP client errors
  strip the request URL before being logged or written to sync history.

**Impact:** an API client cannot read credentials back from the admin API. It
must send a replacement value when changing a sensitive field. Internal DDNS
synchronization continues to use the complete stored configuration.

## DDNS record deletion

**Status:** extension, not present in Go.

`DdnsProvider::delete_record` and `DdnsService::delete_record` add explicit
record deletion. Cloudflare implements it with
`DELETE /zones/{zone}/dns_records/{id}`. `empty` returns an error, and other
providers return an "unsupported" error by default. Callers must not interpret a
no-op provider as proof that a record was deleted.

## GeoIP local database initialization

**Status:** implementation detail.

`mmdb` initialization loads an existing database without downloading, and only
downloads when the file is missing. The admin update path
(`GeoIpService::update_database`) always downloads, matching Go's
`UpdateDatabase`. Initialization failures fall back to `EmptyProvider` rather
than aborting.
