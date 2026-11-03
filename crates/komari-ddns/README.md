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

## Public API redaction (intentional Go deviation)

The database column `ddns_providers.addition` stores the complete provider
configuration, and the DDNS service always reads the complete value. Public
API responses must not expose credentials, so callers should pass stored rows
through `redact_addition` before serializing them:

```rust
use komari_ddns::redact_addition;

let row = db.ddns_providers().get("cloudflare")?.expect("provider");
let response = serde_json::json!({
    "name": row.name,
    // Removes sensitive fields; all other fields are preserved.
    "addition": redact_addition(&row.name, &row.addition),
});
```

For a live provider instance, `redact_provider_configuration(&provider)` does
the same using the instance's declared fields.

Sensitive fields are declared by each provider through
`DdnsProvider::sensitive_fields()` and are also available from the registry as
`DdnsFactory::sensitive_fields(name)`:

| Provider | Removed from API responses | Kept |
| --- | --- | --- |
| `cloudflare` | `api_token`, `zone_id` | `record_id`, `record_type`, `ttl`, `proxied` |
| `webhook` | `url`, `headers`, `body` | `method`, `content_type`, `response_json_path` |
| `empty` | none | all fields |

Behavior:

- Sensitive fields are **removed**, not masked.
- Non-sensitive fields are returned unchanged.
- Invalid JSON, top-level `null`, arrays and other non-object values return
  `{}`; the original text is never echoed.
- Unknown providers return `{}` because their sensitive fields cannot be
  classified.
- Webhook URLs are always removed because Slack/Discord-style webhook paths
  and query strings commonly contain provider-specific tokens, not just
  username/password credentials.
- The input string and the database row are never modified, so internal
  provider use still sees the full configuration.
- `Debug` implementations for Cloudflare and Webhook configurations redact
  their sensitive fields, so logs cannot print tokens, URLs, headers or
  bodies.
- Configuration/header parsing errors use fixed messages that never echo the
  input value, and `reqwest` errors strip the request URL before they reach
  tracing, audit logs or `ddns_sync_history.error`.

This redaction is an intentional deviation from the Go backend, which returns
the raw `addition` from its admin API.

