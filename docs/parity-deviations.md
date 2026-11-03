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
| `webhook` | `headers`, `body`, `password`; `url`/`URL` when it contains userinfo, a credential query parameter, or a Slack/Discord/Teams path token |
| `email` | `password` |
| `Javascript` | `script`, `code` |
| `empty` | none |

**URL handling:** all URL-shaped fields (`url`, `URL`, `server_url`, `endpoint`,
`api_url`) are checked. Credential detection covers userinfo, an exact
credential-query list (`token`, `sig`, `signature`, `x-amz-signature`,
`x-hub-signature-256`, `hmac`, ...), known Slack/Discord/Teams path tokens,
common self-hosted markers (`/token/`, `/key/`, `/secret/`, `/webhook/`,
`/send/`) followed by a token-shaped segment, and Telegram bot tokens embedded
in the path. Token shape means uppercase/mixed case, digits, a longer separated
value, or a very long opaque string; `/send/message`, `/webhook/events` and
`/token/refresh` are preserved. Query/header names are matched exactly after
normalization; unrelated keys such as `author`, `monkey`, `secretary`,
`tokenizer` and `signature_pad` are not removed.

**Debug/logging:** every URL in `Debug` output is reduced to scheme + host +
port (paths and query strings are hidden). Sensitive headers include `Authorization`, `X-Authorization`,
`X-Forwarded-Authorization`, `X-Auth`, `X-Hub-Signature-256`,
`X-Amz-Signature`, `X-Amz-Credential`, `X-Amz-Security-Token`,
`X-Goog-Credential`, `X-Api-Key`, `hmac`, `sig` and similar.
`HttpRequest` bodies and `HttpResponse` bodies are never printed; remote
response bodies/descriptions are not included in errors. Registry type errors
retain only provider, field path and serde error category, never the offending
value or raw diagnostic text.

**Known heuristic limit:** tokens placed in arbitrary path segments of an
unknown self-hosted service are not auto-detected by the API redactor. Debug
output remains safe because it always hides the path.

**Call site:** B06 should call `redact_addition` in the GET/list response path.
Provider configuration writes keep using the original `addition`.

Other B07 deviations (unchanged): the `Javascript` provider is registered but
returns `Unsupported` because goja has no equivalent Rust runtime; email
subjects use RFC 2047 B-encoding instead of Go's Q-encoding; offline online
notifications do not trigger auto-renewal; periodic notification loops are
owned by the caller.
