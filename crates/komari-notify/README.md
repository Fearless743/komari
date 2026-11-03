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
| `email` | parity | Same validation and MIME headers; delivery via lettre (PLAIN/LOGIN, implicit TLS on 465, STARTTLS otherwise). |
| `Server酱³` | parity | JSON `{title, desp, tags?}`. |
| `Server酱Turbo` | parity | JSON `{title, desp, channel?, noip?, openid?}`. |
| `empty` | parity | Always fails with the Go message. |
| `Javascript` | **unsupported** | Go uses `goja`; no Rust runtime with identical semantics. `init`/`send` return `NotifyError::Unsupported` and the provider is documented here instead of silently falling back. |

## Known differences

* `LoadProvider` for `Javascript` fails with `Unsupported`; Go can execute the
  script. This is the only provider intentionally missing.
* Request URLs are validated with `reqwest`/`url`; Go's `url.Parse` accepts a
  few relative forms that fail later in `http.Client.Do`. Absolute URLs used by
  the supported providers behave identically.
* `NotificationService::send_event` templates every event as text. Go's
  optional `IEventMessageSender` is only implemented by the Javascript
  provider, so no supported provider loses event capabilities.
* Offline `OnlineNotification` does not run `utils/renewal` (auto-renewal);
  renewal is outside this task's scope.
* Traffic threshold history is in memory, mirroring Go's `go-cache` (30-day
  TTL). A process restart can re-send the current 5% step once.
* Expiry checks have no per-day "already sent" row, matching Go; cadence comes
  from `next_run` (local 09:00).
