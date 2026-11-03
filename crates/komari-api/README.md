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
| `GET /api/mjpeg_live` | Explicit `501` (see below). |

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

- `/api/mjpeg_live` returns `501 {"status":"error","message":...}` instead of a
  `multipart/x-mixed-replace` stream. Go's handler depends on JSON-RPC node data
  and an externally downloaded MiSans font (`./data/font.ttf`); both are outside
  B04. Clients should fall back to the HTML dashboard until the image pipeline
  is ported.
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
