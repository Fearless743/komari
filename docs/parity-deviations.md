# Parity deviations

This file records intentional behavioural differences between the Rust rewrite
and the Go backend. Unless stated otherwise, everything else follows the Go
implementation.

## Notification provider API redaction (B07)

**Difference:** the Go admin API returns `message_sender_providers.addition`
verbatim, including tokens. The Rust API must return a redacted copy.

**Behaviour:**

- Database rows keep the complete `addition` JSON; internal senders still read
  the full configuration.
- External responses call `komari_notify::redact_addition(provider, &addition)`
  and return its `serde_json::Value`. The redacted value must never be written
  back to the database.
- Sensitive keys are removed, not masked. Non-sensitive keys are preserved.
- Invalid JSON, top-level `null`, non-object values and unknown providers yield
  `{}` without panicking or echoing the original text.

**Sensitive fields:**

| Provider | Removed keys |
| --- | --- |
| `telegram` | `bot_token` |
| `bark` | `device_key` |
| `Server酱³` | `api_url` (contains the sendkey), `sendkey`, `send_key`, `token` |
| `Server酱Turbo` | `api_url` (contains the sendkey), `sendkey`, `send_key`, `token` |
| `webhook` | `headers`, `body`, `password`; `url` when it contains userinfo or credential-like query parameters |
| `email` | `password` |
| `Javascript` | `script`, `code` |
| `empty` | none |

**Call site:** B06 should call `redact_addition` in the GET/list response path.
Provider configuration writes keep using the original `addition`.

Other B07 deviations (unchanged): the `Javascript` provider is registered but
returns `Unsupported` because goja has no equivalent Rust runtime; email
subjects use RFC 2047 B-encoding instead of Go's Q-encoding; offline online
notifications do not trigger auto-renewal; periodic notification loops are
owned by the caller.
