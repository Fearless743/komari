# komari-api — API parity notes

This crate implements the public/browser routes, the agent-facing routes and
the WebSocket hub used by `komari-api`. The following intentional differences
from the Go backend are tracked here so later tasks can adapt them.

## Public API (`public_router`)

Ports of Go `api/public/*.go` plus the route table in `cmd/server.go`:

| Route | Notes |
| --- | --- |
| `ANY /ping` | Plain-text `pong`, matching Go's `r.Any("/ping", ...)`. |
| `GET /api/nodes` | Hidden clients are filtered for guests; `ipv4`, `ipv6`, `remark`, `version` and `token` are always cleared. |
| `GET /api/public` | Defaults match `database.GetPublicInfo`; a valid `temp_key` cookie reports `private_site=false`. |
| `GET /api/version` | `{"version": ..., "hash": ...}` from `KOMARI_VERSION` / `KOMARI_VERSION_HASH` (same as `komari-cli`). |
| `POST /api/login` | Password + optional `2fa_code`; `disable_password_login` returns 403. |
| `GET /api/me` | Raw `{username, logged_in, uuid?, sso_type?, sso_id?, 2fa_enabled?}` (no envelope). |
| `GET /api/logout` | Deletes the session, clears the cookie and redirects to `/`. |
| `GET /api/recent/{uuid}` | Reads the B05 in-memory report buffer (`api.Records`); omits `data` when nothing is buffered. |
| `GET /api/records/load` | `uuid`/`hours`/`load_type` validation, per-metric projection and GPU device grouping. |
| `GET /api/oauth`, `GET /api/oauth_callback` | Uses the B13 provider installed with `ApiState::set_oauth_provider`; error bodies use Go's `error` key. |
| `GET /api/mjpeg_live` | `multipart/x-mixed-replace; boundary=frame` JPEG stream rendered from the latest `records` rows with a built-in bitmap font. Frame interval is `ApiConfig::mjpeg_frame_interval` (2 s by default). |

`public_router_with_state` additionally installs `komari_auth::PrivateSite`,
matching the global private-site middleware in `cmd/server.go`. Server
assemblies that already apply `PrivateSite` globally can use
`public_router()` directly.

OAuth is not loaded automatically. Call
`komari_api::public::initialize_oauth(&state).await` (which seeds default
provider rows and loads `o_auth_provider`) before serving requests.

## Implemented Go semantics

- HTTP reports are buffered in memory and aggregated every minute with Go's
  top-30% `AverageReport` / `AverageGPUReports` rules before one row is written
  to `records` / `gpu_records`. Samples inside the retention window stay in
  the buffer after a flush, matching Go's `Records.Set(filtered, ...)`; B07 RPC
  work can read this recent buffer. Tests use `ApiState::flush_reports` for a
  deterministic flush.
- `cpu.usage < 0.01` is clamped to `0.01` before appending to the aggregation
  buffer (Go `SaveClientReport`) and again defensively on the database write
  path (before `ReportVerify`). The latest-report cache keeps the raw value.
- Static totals (`ram_total`, `swap_total`, `disk_total`) come from the sample
  with the highest UDP connection count, matching the final sort in Go's
  `AverageReport`.
- One aggregated `records` row and its `gpu_records` rows are written in a
  single SQLite transaction, so a failed retry cannot duplicate GPU samples.
- `allow_cors` is read from the database on every browser WebSocket upgrade;
  `KOMARI_WS_DISABLE_ORIGIN` only accepts a case-insensitive `true` (like Go's
  `strings.EqualFold(value, "true")`). Values such as `1`, `yes`, `on`, or
  ` true ` do not disable the check.
- Report POST tokens come from `?token=` or a JSON body `token` for non-GET
  requests. `Authorization: Bearer` is reserved for the admin API key, matching
  B03's `AuthLayer`.
- `/api/clients/*` accepts both `Admin` and `Client` roles; report bodies may
  target a client UUID for administrators.
- `uploadBasicInfo` reads `X-Forwarded-For`, `X-Real-IP`, then the socket peer
  address, and writes the GeoIP region emoji when `geo_ip_enabled` is true and
  a `GeoIpService` is installed on `ApiState`.

## Known differences / follow-up work

- `/api/mjpeg_live` renders from the `records` table instead of the JSON-RPC
  node snapshot, uses the built-in 5x7 bitmap font instead of the downloaded
  MiSans font, and drops hidden clients like Go's guest `common:getNodes` call.
  The `lang` and `tz_offset` query parameters are accepted but the frame text is
  English and uses the application timezone.
- `/api/records/load` rejects `hours` outside `±2_562_047` (the largest whole
  hour offset that fits in Go's `time.Duration`) with 400. Go silently wraps the
  duration for larger values.
- `/api/public` persists missing defaults with `get_as_or`, matching Go's
  `GetManyAs[config.Legacy]` write-back. One value differs: Rust stores the
  `GetPublicInfo` description default (`Komari Monitor, a simple server
  monitoring tool.`), while Go first writes the `Legacy` struct-tag default
  (`A simple server monitor tool.`) and then reads it back. Subsequent calls in
  both implementations return the stored value.
- `client_ip` falls back to `ConnectInfo<SocketAddr>` after `X-Forwarded-For`
  and `X-Real-IP`. The server assembly must use
  `into_make_service_with_connect_info` to match Go's `RemoteAddr` fallback.
- `/api/rpc2` and the admin API are separate tasks (B06/B07) and are not part of
  this router.
- `/api/oauth_callback` binding uses `komari_oauth::AccountLinker::bind`, which
  also records `users.sso_type` and rejects an identity already owned by
  another account. Go only writes `sso_id`.
- `uploadBasicInfo` accepts a POST body `token` and an administrator body
  `uuid`. Go only reads the query token and returns 400 without it. This is an
  intentional extension for the B05 checklist; strict Go parity would reject
  both.
- `uploadBasicInfo` is deliberately more lenient than Go: missing numeric
  fields and unknown JSON keys are ignored instead of causing a Go panic/SQL
  error. Required-field strictness can be tightened once the agent contract is
  finalized.
- GeoIP is not initialized automatically by `ApiState::new`; the server
  assembly must call `GeoIpService::initialize` and install it with
  `ApiState::with_geoip_service`.
- Browser `GET /api/clients` does not yet apply login/hidden-client filtering;
  that belongs to the web task (B09).
- An administrator WebSocket still needs a resolvable `?token=` client token;
  admin HTTP reports/basic-info use the body UUID instead.

## B06 — admin API and JSON-RPC 2.0

`admin_router()` implements the Go `api/admin/**` groups (settings, clients,
records, sessions, 2FA, backup, DDNS/notification/OAuth providers, themes,
logs, clipboard, notification rules, test/update endpoints). Every handler
takes `RequireAdmin`, so merging `admin_router()` without the role middleware
still returns `401` for guests and agents.

`rpc_router()` implements `/api/rpc2` over both POST and WebSocket. Both
transports share `rpc::dispatch`, the namespace checks (`guest`/`client`/
`admin`/`common`) and the codes from `utils/rpc/errors.go`. Registered methods:

- `common:getNodes`, `common:getNodesLatestStatus`, `common:getMe`,
  `common:getPublicInfo`, `common:getVersion`, `common:getNodeRecentStatus`,
  `common:getRecords`
- internal `rpc.ping`, `rpc.version`, `rpc.methods`, `rpc.help`

The Go backend has no `admin:*` RPC methods, so they are **not** in the default
registry. Set `KOMARI_RPC_ADMIN_METHODS=true` to opt in to the extension
methods (`admin:getClients`, `admin:getClient`, `admin:getSettings`,
`admin:saveSettings`, `admin:addClient`, `admin:editClient`,
`admin:removeClient`); `rpc.methods` then lists them too.

Provider GET responses call `komari_ddns::redact_addition`,
`komari_notify::redact_addition` or the local OAuth redactor; the database
keeps the complete `addition` JSON.

Known differences:

- `GET /api/admin/2fa/generate` returns `image/png` QR code bytes and the
  `2fa_secret` cookie, matching Go. The otpauth URL is not returned as JSON.
- Theme upload/update/import endpoints are not implemented (listing, switching,
  deleting and per-theme settings are). They need ZIP/SSRF handling from the Go
  implementation.
- `common:getRecords` iterates the current `clients` table, so records of
  already-deleted clients are not returned.
- `common:getPublicInfo` does not merge managed-theme defaults from
  `data/theme/<short>/komari-theme.json`.
- `common:getMe` for an agent returns the token in `uuid`, matching the
  observable Go behaviour (its lookup error check is inverted).
- Admin backup upload validates `komari-backup-markup` and stages
  `<data_dir>/backup.zip`; the server assembly must call
  `komari_db::restore_pending_backup` at startup (Go exits and restores on the
  next boot).

## 历史提交说明

首轮 B06 提交 `217231a`（`feat(api): 增加管理 API 的设置与客户端端点`）在
`admin/mod.rs` 中提前声明了后续提交才加入的模块，因此该提交单独检出时不可
编译。后续历史（`b6e467f`、`29e55c2`、`a458e2c`、`2e15cd4`）同样依赖前序
提交才能构建。仓库未重写这些历史提交；自复审修复提交 `3392fbb` 起，每笔提交
均在提交后执行 `cargo build -p komari-api` 验证，可单独构建。
