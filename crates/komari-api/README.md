# komari-api — agent API parity notes

This crate implements the agent-facing routes and the WebSocket hub used by
`komari-api`. The following intentional differences from the Go backend are
tracked here so later tasks can adapt them.

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
  `KOMARI_WS_DISABLE_ORIGIN` only accepts a case-insensitive `true`.
- Report POST tokens come from `?token=` or a JSON body `token` for non-GET
  requests. `Authorization: Bearer` is reserved for the admin API key, matching
  B03's `AuthLayer`.
- `/api/clients/*` accepts both `Admin` and `Client` roles; report bodies may
  target a client UUID for administrators.
- `uploadBasicInfo` reads `X-Forwarded-For`, `X-Real-IP`, then the socket peer
  address, and writes the GeoIP region emoji when `geo_ip_enabled` is true and
  a `GeoIpService` is installed on `ApiState`.

## Known differences / follow-up work

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
