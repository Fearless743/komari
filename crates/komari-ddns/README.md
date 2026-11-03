# komari-ddns

DDNS providers and synchronization for Komari. The crate mirrors the Go
packages under `utils/ddns`, including the `DdnsProvider` interface, provider
factory/registry, Cloudflare/Webhook/Empty providers, retry logic and
`ddns_sync_history` writes.

## Non-Go extension: record deletion

Go's `factory.IDdnsProvider` only creates and updates records through `Sync`.
This crate adds an optional asynchronous `DdnsProvider::delete_record(record_id)`
so callers can clean up records without overloading `Sync`:

- `cloudflare` implements it with `DELETE /zones/{zone}/dns_records/{id}`.
- `empty` returns an error instead of claiming success; the empty provider has
  no backend and cannot delete anything.
- Every other provider keeps the default implementation, which returns an
  "unsupported" error.
- `DdnsService::delete_record` requires a loaded provider and simply delegates.

Callers must treat a successful `delete_record` as provider-specific and must
not assume that an `empty` provider or a no-op provider deleted a record. This
API is intentionally an extension and is **not** part of Go feature parity.
