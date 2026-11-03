# komari-rs

Rust reimplementation of the Komari backend. The Go implementation in
`../komari` remains as a fallback until feature parity is verified.

## Status

This workspace currently provides the project skeleton, configuration loading,
version reporting, and the command-line interface. The HTTP server and business
logic live in follow-up tasks; every crate below already exists so later work
only changes the crate it owns.

| Crate | Responsibility |
| --- | --- |
| `komari-cli` | `komari` binary, flags, logging, version |
| `komari-config` | Environment/flag configuration and site defaults |
| `komari-db` | Database layer |
| `komari-auth` | Authentication |
| `komari-api` | HTTP/JSON-RPC API |
| `komari-notify` | Notification delivery |
| `komari-jobs` | Scheduled work |
| `komari-ddns` | DDNS integrations |
| `komari-geoip` | GeoIP lookups |
| `komari-oauth` | OAuth/OIDC |
| `komari-web` | Static themes and web assets |

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
| `KOMARI_WS_DISABLE_ORIGIN` | `--ws-disable-origin[=BOOL]` | `false` |

Empty environment values are treated as unset, matching the Go `GetEnv`
helper. Boolean values accept `true/false`, `1/0`, `yes/no`, and `on/off`.

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

`chpasswd` writes a bcrypt hash to the SQLite database and deletes all login
sessions. Without `--username` it changes the first account, matching the Go
command. `disable-2fa` clears one user's TOTP secret, or every user's secret
when no username is given. `permit-login` sets `disable_password_login` to
`false`. All three use `--database` / `KOMARI_DB_FILE` and do not create a
missing database file.
