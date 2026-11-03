# komari-api — agent API parity notes

This crate implements the agent-facing routes and the WebSocket hub used by
`komari-api`. The following intentional differences from the Go backend are
tracked here so later tasks can adapt them.

## Implemented Go semantics

- HTTP reports are buffered in memory and aggregated every minute with Go's
  top-30% `AverageReport` / `AverageGPUReports` rules before one row is written
  to `records` / `gpu_records`. Tests use `ApiState::flush_reports` for a
  deterministic flush.
- `cpu.usage < 0.01` is clamped to `0.01` on the database write path (before
  `ReportVerify`), while the latest-report cache keeps the raw value.
- `allow_cors` is read from the database on every browser WebSocket upgrade;
  `KOMARI_WS_DISABLE_ORIGIN` only accepts a case-insensitive `true`.
- Agent POST tokens come from `?token=` or a JSON body `token` for non-GET
  requests. `Authorization: Bearer` is reserved for the admin API key, matching
  B03's `AuthLayer`.
- `/api/clients/*` accepts both `Admin` and `Client` roles. Agents update their
  own UUID; administrators may target a client through the report/basic-info
  body UUID.
- `uploadBasicInfo` reads `X-Forwarded-For`, `X-Real-IP`, then the socket peer
  address, and writes the GeoIP region emoji when `geo_ip_enabled` is true and
  a `GeoIpService` is installed on `ApiState`.

## Known differences / follow-up work

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
