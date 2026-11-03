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
