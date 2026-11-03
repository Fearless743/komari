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


## 对外 API 脱敏

数据库中的 `message_sender_providers.addition` 始终保存完整配置，内部发送也
继续读取完整配置。只有**对外返回** provider 配置时，才调用：

```rust
use komari_notify::redact_addition;

// B06 管理 API 的 GET 响应路径：
let safe = redact_addition("telegram", &provider.addition);
// 将 safe 作为 JSON 返回；不要把 safe 写回数据库。
```

`redact_addition` 会解析 `addition`，删除敏感键后返回 `serde_json::Value`。
非法 JSON、顶层 `null`、数组/字符串/数字等非对象值、未知 provider 都返回
`{}`，不会 panic，也不会回显原始字符串。键名比较忽略大小写、下划线和连字符。

| Provider | 移除的字段 |
| --- | --- |
| `telegram` | `bot_token` |
| `bark` | `device_key` |
| `Server酱³` | `api_url`（内含 sendkey），以及 `sendkey` / `send_key` / `token` 别名 |
| `Server酱Turbo` | `api_url`（内含 sendkey），以及 `sendkey` / `send_key` / `token` 别名 |
| `webhook` | `headers`、`body`、`password`；`url` 含 userinfo 或 token/secret/key 查询参数时也移除 |
| `email` | `password` |
| `Javascript` | `script`、`code` |
| `empty` | 无 |

配置结构体的 `Debug` 输出同样经过脱敏；`HttpRequest` 的 `Debug` 不显示请求体
和敏感请求头，`EmailEnvelope` 不显示密码和 MIME 原文。

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
