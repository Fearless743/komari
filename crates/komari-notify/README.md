# komari-notify

Rust port of the Go backend's notification stack:

* `utils/messageSender/{factory,loader}.go` → `provider`, `registry`
* `utils/messageSender/<provider>` → `providers/*`
* `utils/messageSender/sender.go` → `event`, `service`
* `utils/notifier/{offline,traffic,expire,load}.go` → `notifier/*`
* `database/notification` + `database/messageSender.go` state is read/written
  through `komari-db`.

## Providers

| Name | Status | Notes |
| --- | --- | --- |
| `telegram` | parity | `POST endpoint + bot_token + /sendMessage`, form fields sorted like Go `url.Values.Encode`. |
| `bark` | parity | JSON `{body, device_key, title?, icon?, level?}` to `<server>/push`. |
| `webhook` | parity | GET/POST templates, `\uXXXX` JSON escaping, custom headers, basic auth. |
| `email` | parity | Same validation and MIME headers; delivery via lettre (PLAIN/LOGIN; implicit TLS on 465, required STARTTLS for other `use_ssl=true` ports, opportunistic STARTTLS when `use_ssl=false`, like Go `smtp.SendMail`). |
| `Server酱³` | parity | JSON `{title, desp, tags?}`. |
| `Server酱Turbo` | parity | JSON `{title, desp, channel?, noip?, openid?}`. |
| `empty` | parity | Always fails with the Go message. |
| `Javascript` | **unsupported** | Go uses `goja`; no Rust runtime with identical semantics. `init`/`send` return `NotifyError::Unsupported` and the provider is documented here instead of silently falling back. |

## Known differences

* `LoadProvider` for `Javascript` fails with `Unsupported`; Go can execute the
  script. This is the only provider intentionally missing.
* Provider `Default` values are Go's zero-value `Addition` structs. UI defaults
  (telegram endpoint, bark server/level, webhook method/content-type/body,
  email port 587) are exposed only through `ConfigField::default` and are not
  silently injected into stored JSON.
* `addition` handling matches Go's `json.Unmarshal`: top-level `null` and
  `null` fields fall back to zero values, while an empty string is an
  `InvalidConfig` ("unexpected end of JSON input").
* Timeouts follow Go: telegram and bark use no client timeout
  (`http.DefaultClient`); webhook, Server酱³ and Server酱Turbo use 30 seconds.
* Email subjects use RFC 2047 B-encoding from lettre. Go's `mime.QEncoding`
  uses Q-encoding. Both are valid and decode to the same subject; bytes differ.
* `notifier::load::run_all_load_tasks` is a direct evaluation helper. Go's
  `LoadNotificationService` interval tickers / `Reload` / `stopChan` are not
  started implicitly; the caller owns periodic scheduling (as with offline,
  traffic and expiry, whose loops also live outside this crate).
* Request URLs are validated with `reqwest`/`url`; Go's `url.Parse` accepts a
  few relative forms that fail later in `http.Client.Do`. Absolute URLs used by
  the supported providers behave identically.
* `NotificationService::send_event` templates every event as text. Go's
  optional `IEventMessageSender` is only implemented by the Javascript
  provider, so no supported provider loses event capabilities.
* Offline `OnlineNotification` does not run `utils/renewal` (auto-renewal);
  renewal is outside this task's scope.
* Traffic threshold history is in memory and expires lazily after 30 days,
  mirroring Go's `go-cache` TTL. A process restart can re-send the current 5%
  step once.
* Expiry checks have no per-day "already sent" row, matching Go; cadence comes
  from `next_run` (local 09:00).
