# komari-rs

Rust reimplementation of the Komari backend. The Go implementation in
`../komari` remains as a fallback until feature parity is verified.

## Status

The merged baseline provides the CLI/configuration, the SQLite persistence
layer, authentication, notifications, GeoIP, DDNS, scheduled record maintenance,
the B14 static web assets (`komari-web`) and the B13 OAuth/OIDC crate
(`komari-oauth`). The HTTP server, public/admin API and agent WebSocket assembly
remain B04/B05/B06; OAuth HTTP login/binding routes and B14 static routes still
need server assembly. `komari server` currently resolves configuration and
prints it without binding a port.

| Crate | Responsibility | Status |
| --- | --- | --- |
| `komari-cli` | `komari` binary, flags, logging, version | merged (B01) |
| `komari-config` | Environment/flag configuration and site defaults | merged (B01) |
| `komari-db` | SQLite database layer | merged (B02) |
| `komari-auth` | Authentication, sessions, TOTP, roles | merged (B03) |
| `komari-api` | HTTP/JSON-RPC API | placeholder; B04/B05/B06 |
| `komari-notify` | Notification delivery | merged (B07) |
| `komari-jobs` | Scheduled record maintenance | merged |
| `komari-ddns` | DDNS integrations | merged |
| `komari-geoip` | GeoIP lookups | merged (B08) |
| `komari-oauth` | OAuth/OIDC | merged (B13); HTTP routes pending assembly |
| `komari-web` | Static themes and web assets | merged (B14); server assembly pending |

## Documentation

| Document | Contents |
| --- | --- |
| [docs/CONFIGURATION.md](docs/CONFIGURATION.md) | All `KOMARI_*` variables, CLI flags, defaults, database, logging, CORS/WS, GeoIP/DDNS/OAuth/notification keys |
| [docs/MIGRATION.md](docs/MIGRATION.md) | Go → Rust migration, SQLite schema compatibility, service swap, rollback, version injection, frontend assets |
| [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) | systemd, Docker/Compose, reverse proxy (WebSocket/body/timeouts), backups, health checks, port 25774 |
| [docs/API.md](docs/API.md) | Go reference routes and Rust implementation status (public/client/admin/rpc2/static) |
| [docs/PARITY.md](docs/PARITY.md) | Module status and D-number summary |
| [docs/parity-deviations.md](docs/parity-deviations.md) | Known Go/Rust behavior differences |

Run `./docs/check-docs.sh` to verify documented environment variables, flags,
configuration keys and relative links against the code.

## Build

Requires a Rust toolchain with edition 2024 support (Rust 1.85 or newer).

```sh
cargo build
cargo test
cargo run -p komari-cli -- --help
```

The binary is named `komari`:

```sh
cargo run -p komari-cli -- server --listen 0.0.0.0:25774
```

Passing no subcommand is equivalent to `server`, matching the Go CLI.

## Configuration

Precedence is **command-line flag > environment variable > built-in default**.

| Environment variable | Flag | Default |
| --- | --- | --- |
| `KOMARI_LISTEN` | `--listen`, `-l` | `0.0.0.0:25774` |
| `KOMARI_DB_TYPE` | `--db-type`, `-t` | `sqlite` |
| `KOMARI_DB_FILE` | `--database`, `-d` | `data/komari.db` |
| `KOMARI_DB_HOST` | `--db-host` | `localhost` |
| `KOMARI_DB_PORT` | `--db-port` | `3306` |
| `KOMARI_DB_USER` | `--db-user` | `root` |
| `KOMARI_DB_PASS` | `--db-pass` | empty |
| `KOMARI_DB_NAME` | `--db-name` | `komari` |
| `KOMARI_LOG_LEVEL` | `--log-level` | `debug` for development builds, otherwise `info` |
| `KOMARI_THEME` | `--theme` | `default` |
| `KOMARI_ALLOW_CORS` | `--allow-cors[=BOOL]` | `false` |
| `KOMARI_ENABLE_CLOUDFLARED` | `--enable-cloudflared[=BOOL]` | `false` |
| `KOMARI_CLOUDFLARED_TOKEN` | none | unset |
| `KOMARI_WS_DISABLE_ORIGIN` | `--ws-disable-origin[=BOOL]` | `false` |

Empty environment values are treated as unset, matching the Go `GetEnv`
helper. Environment booleans accept `true/false`, `1/0`, `yes/no`, and
`on/off`; CLI boolean flags accept only lowercase `true`/`false` (bare
`--flag` means true). The full reference, including persisted `configs` keys
and provider JSON, is in [docs/CONFIGURATION.md](docs/CONFIGURATION.md).

## Version

`--version` reports `CARGO_PKG_VERSION` by default. Release builds can override
both values at compile time:

```sh
KOMARI_VERSION=1.2.3 KOMARI_VERSION_HASH=abcdef0 cargo build --release
```

When `KOMARI_VERSION_HASH` is absent it reports `unknown`, which selects debug
logging by default, matching the Go backend's `utils.VersionHash` behavior.

## Commands

```text
komari server
komari chpasswd -p <password> [-u <username>]
komari disable-2fa [-u <username>]     # alias: disable2FA
komari permit-login                    # alias: permitPasswordLogin
```

`chpasswd` writes the Go-compatible SHA-256 password hash
(`base64(SHA-256(password + "06Wm4Jv1Hkxx"))`) to the SQLite database and
deletes all login sessions, so the Go fallback can still authenticate. Without
`--username` it changes the first account by UUID, matching the Go command.
`disable-2fa` clears one user's TOTP secret, or every user's secret when no
username is given. `permit-login` sets `disable_password_login` to `false`.
`disable-2fa`/`permit-login` create and migrate a missing database like Go;
`chpasswd` keeps Go's "Database file does not exist." message instead.
`permit-login` writes the current key/value `configs` row; the Go command's
legacy single-row column update only matters for pre-1.1.4 schemas.
