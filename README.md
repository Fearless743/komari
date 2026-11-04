# komari-rs

Rust reimplementation of the Komari backend. The Go implementation in
`../komari` remains as a fallback until feature parity is verified.

## Status

The merged baseline provides the CLI/configuration, the SQLite persistence
layer, authentication, notifications, GeoIP, DDNS, scheduled record maintenance,
the B14 static web assets (`komari-web`) and the B13 OAuth/OIDC crate
(`komari-oauth`). B09 adds the HTTP server assembly: `komari server` binds
`KOMARI_LISTEN` (default `0.0.0.0:25774`), mounts the agent/browser WebSocket
routes, OAuth login/callback, static assets and SPA fallback, restores a pending
`data/backup.zip`, and starts the background jobs. The public API/RPC2 (B04)
and admin API (B06) are mounted by default through the same router assembly.
At startup GeoIP, DDNS, notification and OAuth providers are initialized; the
browser WebSocket filters `hidden` clients for guests and honours private-site
mode. `ApiState.web` / `ApiState.ddns` share the live `WebAssets` and
`DdnsService` handles with B06, backup uploads use a separate body limit, and
the listener installs `ConnectInfo<SocketAddr>` so peer-IP fallbacks work.

| Crate | Responsibility | Status |
| --- | --- | --- |
| `komari-cli` | `komari` binary, flags, logging, version | merged (B01); server binds in B09 |
| `komari-config` | Environment/flag configuration and site defaults | merged (B01) |
| `komari-db` | SQLite database layer | merged (B02) |
| `komari-auth` | Authentication, sessions, TOTP, roles | merged (B03) |
| `komari-api` | Agent/browser, public, admin, RPC and HTTP server assembly | merged (B04/B05/B06/B09) |
| `komari-notify` | Notification delivery and auto-renewal | merged (B07) |
| `komari-jobs` | Scheduled record maintenance | merged |
| `komari-ddns` | DDNS integrations | merged |
| `komari-geoip` | GeoIP lookups | merged (B08) |
| `komari-oauth` | OAuth/OIDC | merged (B13); HTTP routes assembled by B09 |
| `komari-web` | Static themes and web assets | merged (B14); routes assembled by B09 |

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

## 安装

`install.sh`（移植自 Go 版 `install-komari.sh`）提供交互式菜单：安装 / 升级 /
卸载 / 状态 / 日志 / 重启 / 停止。安装位置、服务名、数据目录与 Go 版完全一致，
可直接覆盖升级：

```sh
curl -fsSLO https://raw.githubusercontent.com/Fearless743/komari/main/install.sh
chmod +x install.sh
sudo ./install.sh
```

- 安装目录 `/opt/komari`，二进制 `/opt/komari/komari`，systemd 服务 `komari`，
  `WorkingDirectory=/opt/komari`，数据库沿用 `./data/komari.db`（见
  [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) 第 2、3 节）。
- 从 Release `latest` 下载 `komari-linux-amd64` / `komari-linux-arm64`；
  **386 / riscv64 无 Rust 预编译包**，脚本会明确报错（可用 Go 版 v0.2.0 或自行构建）。
- 全新安装后会自动从服务日志提取初始管理员密码（Rust 日志格式优先，兼容 Go 格式）。
- 升级前会自动备份旧二进制（`komari.backup.<时间戳>`）；数据目录不受影响，
  数据库迁移在启动时自动执行。

从 Go 版迁移（建议先 Agent 后面板，Rust Agent 只支持 v1 协议）：在同一台机器上
直接运行上面的脚本选“升级”，服务定义会被重写为指向新二进制，数据库原地兼容。
回退时把备份的旧二进制拷回即可，详见 [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md)
第 9 节与 [docs/MIGRATION.md](docs/MIGRATION.md)。

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
