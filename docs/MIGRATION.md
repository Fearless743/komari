# 从 Go 版迁移到 Rust 版

本文档描述如何把现有 Komari Go 部署迁移到 `komari-rs`，以及如何回退。
迁移的核心原则是：**同一个 SQLite 文件、同一个数据目录、同一个监听端口
`25774`**。Rust 版读取 Go/GORM 生成的 schema，不要求导出/导入。

> **当前实现边界（B11 基线）**
>
> `komari server` 目前只解析配置并打印启动信息，HTTP/API/Web/WS/OAuth 尚未装配
> （B04/B05/B06，以及 B13 OAuth HTTP/B14 静态路由的服务器装配）。因此本指南的
> “切换二进制”步骤只在数据/服务层面成立；在 HTTP 层合并前，生产流量应继续由
> Go 版承载。模块状态见 [PARITY.md](PARITY.md)。

## 1. 迁移前检查

1. 记录当前 Go 版本、启动参数、环境变量和 systemd/Docker 配置。
2. 确认数据库是 SQLite，而不是历史 MySQL 尝试：
   ```sh
   ls -l data/komari.db
   sqlite3 data/komari.db 'PRAGMA journal_mode;'
   ```
3. 确认数据目录包含主题、favicon、GeoIP 数据库等：
   ```sh
   find data -maxdepth 2 -type f | sort
   ```
4. 停掉写入方。SQLite 同一时间只应有一个服务进程写入；不要在 Go 和 Rust
   同时运行时切换。
5. 完整备份，见第 3 节。

## 2. 构建与安装 Rust 二进制

要求 Rust 1.85+（edition 2024）。

```sh
cargo build --release -p komari-cli
install -m 0755 target/release/komari /opt/komari/komari
```

发布构建建议在编译时注入版本：

```sh
KOMARI_VERSION=1.2.3 \
KOMARI_VERSION_HASH=$(git rev-parse --short HEAD) \
cargo build --release -p komari-cli
```

说明：

- `KOMARI_VERSION` 覆盖 `CARGO_PKG_VERSION`（当前 workspace 为 `0.1.0`）。
- `KOMARI_VERSION_HASH` 缺省为 `unknown`，会被视为开发构建，默认日志级别变为
  `debug`；启动日志会打印 `hash`。
- 这两个变量是 **编译期** 变量（`option_env!`），运行时设置无效。
- `komari --version` 输出 `KOMARI_VERSION`（或 Cargo 版本）；启动 banner 才包含
  hash。
- 对应 Go 的 ldflags：
  `-X .../utils.CurrentVersion=... -X .../utils.VersionHash=...`。

保留 Go 二进制用于回退：

```sh
install -m 0755 /usr/local/bin/komari /opt/komari/komari-go
```

## 3. 备份

### 推荐：停机 + SQLite 在线备份

```sh
systemctl stop komari
install -d -m 0750 /var/backups/komari
sqlite3 /opt/komari/data/komari.db \
  ".backup '/var/backups/komari/komari-$(date +%F-%H%M%S).db'"
tar -C /opt/komari -czf /var/backups/komari/data-$(date +%F-%H%M%S).tar.gz \
  data/theme data/favicon.ico data/GeoLite2-Country.mmdb 2>/dev/null || true
```

如果必须在线备份，`sqlite3 .backup` 可以在 WAL 模式下安全执行；不要只复制
`komari.db` 而漏掉 `komari.db-wal` / `komari.db-shm`，除非先停机并让 SQLite
完成 checkpoint。

### 恢复

先停服务，再同时恢复数据库和 `data/` 资源：

```sh
systemctl stop komari
cp /var/backups/komari/komari-YYYY-MM-DD-HHMMSS.db /opt/komari/data/komari.db
rm -f /opt/komari/data/komari.db-wal /opt/komari/data/komari.db-shm
# 恢复主题/favicon/mmdb；如果归档包含这些路径
tar -xzf /var/backups/komari/data-YYYY-MM-DD-HHMMSS.tar.gz -C /opt/komari
chown -R komari:komari /opt/komari/data
systemctl start komari
```

如果只备份了单个文件，则分别复制：

```sh
cp -a /var/backups/komari/theme /opt/komari/data/theme
cp -a /var/backups/komari/favicon.ico /opt/komari/data/favicon.ico
cp -a /var/backups/komari/GeoLite2-Country.mmdb /opt/komari/data/GeoLite2-Country.mmdb
chown -R komari:komari /opt/komari/data
```

## 4. 数据库 schema 兼容性

Rust 的 `komari-db` 直接读写 Go/GORM 生成的 SQLite schema：

- Rust 管理的 17 张表（`users`、`clients`、`sessions`、`records`、
  `gpu_records`、`logs`、`clipboards`、`offline_notifications`、
  `load_notifications`、`oidc_providers`、`message_sender_providers`、
  `ddns_providers`、`ddns_sync_history`、`theme_configurations`、`configs`、
  `records_long_term`、`gpu_records_long_term`）的列、索引、约束与参考库对齐；
- 参考库还可能包含 `command_audits`、`ping_records`、`ping_tasks`、
  `task_results`、`tasks` 等表；Rust **不创建、不迁移**这些表，回退时需保留，
  或继续由 Go 管理；
- `migrate()` 只做 `CREATE TABLE/INDEX IF NOT EXISTS`、缺失列
  `ALTER TABLE ... ADD COLUMN`、缺失唯一索引补齐；历史迁移与 legacy
  `configs` 转换在同一个事务中执行，失败会整体回滚；
- 旧版单行 `configs` 表会转换为 key/value 形式，原表保留为
  `configs_legacy_backup`；
- 旧版 `client_infos` 会合并到 `clients`，原表重命名为
  `client_infos_backup`；pre-1.0.2 的 OAuth/通知配置会迁移到
  `oidc_providers` / `message_sender_providers`，与 Go `MergeDatabase` 对齐；
  旧凭据列不会进入新的 key/value `configs`，只保留在 `configs_legacy_backup`。

验证 schema 兼容性（使用数据库副本，避免污染生产库）：

```sh
cp data/komari.db /tmp/komari-compat.db
KOMARI_COMPAT_DB=/tmp/komari-compat.db cargo test -p komari-db --test compat
```

`tests/compat.rs` 会在文件存在时逐表比较 `PRAGMA table_info` 和
`PRAGMA index_list`；文件不存在时跳过。

### 非常老的数据库

Rust 会直接执行 Go 的历史迁移，因此以下数据库不需要先用 Go 启动：

- pre-1.0.2：OAuth/通知配置仍在旧 `configs` 列中；
- 仍有 `client_infos` 表、尚未合并到 `clients`；
- 仍使用单行 `configs` 且包含 `id` 列。

迁移在单个事务中完成；失败不会修改原库。建议切换前仍先备份 `data/komari.db`，
迁移后确认 `client_infos_backup` / `configs_legacy_backup` 内容再删除。

## 5. 替换服务

### systemd

把 `ExecStart` 从 Go 二进制改为 Rust 二进制，保持 `WorkingDirectory` 不变，
因为默认数据库路径 `data/komari.db` 是相对路径：

```ini
[Service]
WorkingDirectory=/opt/komari
EnvironmentFile=-/etc/komari/komari.env
ExecStart=/opt/komari/komari server
```

```sh
systemctl daemon-reload
systemctl start komari
journalctl -u komari -f
```

启动日志应包含：

- `Komari Monitor`、`version`、`hash`；
- `startup configuration`，其中 `db_file` 指向现有数据库；
- `Starting server on 0.0.0.0:25774 ...`。

> 当前基线只打印最后一行，不会真正 bind 端口；HTTP 层合并后才会监听。

### Docker / Compose

容器细节以仓库根目录的 `Dockerfile` 和 `docker-compose.yml` 为准（B10 已合并）。
迁移时保持数据卷指向原数据：

```yaml
volumes:
  - komari-data:/app/data
# 或者 bind mount（先创建并修正 uid 10001 权限）：
# - ./data:/app/data
environment:
  KOMARI_DB_FILE: /app/data/komari.db
```

说明：

- B10 已合并；镜像使用非 root 用户（uid/gid `10001`）。bind mount 需要
  `chown -R 10001:10001 ./data`，命名卷会继承镜像内 `/app/data` 的属主。
- `KOMARI_THEME_SRC` 是 B10 Dockerfile 的构建参数（相对构建上下文），默认
  `crates/komari-web/defaultTheme`，会被复制到 `/src/theme`。
- `KOMARI_DEFAULT_THEME_DIR` 是 B14 的**编译期**变量
  （`crates/komari-web/build.rs`），Docker 默认 `/src/theme`；不是 compose 运行时
  环境变量。以上变量都不由 `komari-config` 读取。
- 镜像当前不包含 `curl`/`wget`；不要照搬需要这些工具的 compose healthcheck。
  健康检查应由宿主机/外部监控执行，或按 `Dockerfile` 实际安装的工具调整。
- 不要同时启动 Go 和 Rust 容器写同一个 SQLite 文件。

## 6. 切换后验证

在 HTTP 层可用后，按以下顺序验证：

1. 健康检查：`curl -fsS http://127.0.0.1:25774/ping` 返回 `pong`。
2. 公开接口：`GET /api/version`、`GET /api/public`。
3. 登录：`POST /api/login`，检查 `session_token` cookie。
4. 管理员接口：`GET /api/admin/settings/`（需要管理员会话或 API key）。
5. agent 注册与上报：`POST /api/clients/register`，然后
   `GET /api/clients/report` WebSocket 或 `POST /api/clients/report`。
6. 浏览器实时数据：`GET /api/clients` WebSocket。
7. 通知、DDNS、GeoIP：检查日志与数据库写入。

当前基线可验证的部分：

```sh
# 配置解析
komari server --database /path/to/komari.db --log-level debug

# 数据库 schema
cargo test -p komari-db

# 认证/CLI 辅助命令
komari chpasswd --username admin --password 'new-password'
komari disable-2fa --username admin
komari permit-login
```

## 7. 回退到 Go

Rust 的迁移是加法式的，但旧 `configs` 转换、缺失列补齐仍会改变文件。最稳妥的
回退是恢复迁移前备份：

```sh
systemctl stop komari
cp /var/backups/komari/komari-BEFORE-MIGRATION.db /opt/komari/data/komari.db
rm -f /opt/komari/data/komari.db-wal /opt/komari/data/komari.db-shm
# 把 ExecStart 改回 /opt/komari/komari-go server ...
systemctl daemon-reload
systemctl start komari
```

如果确认没有执行旧配置转换，也可以直接用 Go 打开 Rust 迁移后的数据库；Go 的
`AutoMigrate` 会保留新增列。但涉及 `configs_legacy_backup` 或历史迁移时，恢复
备份更安全。

回退检查：

- `/ping` 返回 `pong`；
- 管理员能登录；
- agent 能继续上报；
- 通知/DDNS 配置仍存在。

## 8. 已知行为差异

完整登记见 [parity-deviations.md](parity-deviations.md)，摘要见
[PARITY.md](PARITY.md)。迁移时重点关注：

| 编号 | 影响 | 迁移建议 |
| --- | --- | --- |
| D-001 | `Javascript` 通知 provider 不可用 | 改用 telegram/bark/webhook/email 等原生 provider。 |
| D-002 | DDNS `delete_record` 是 Rust 扩展 | 不要依赖 Go 提供该能力；仅 cloudflare 实现。 |
| D-003 | 旧库历史迁移（已合并） | 直接切换即可；迁移失败会回滚。 |
| D-004 | GeoIP 缓存上限 10,000 | 大量唯一 IP 时可能增加上游查询。 |
| D-005 | `KOMARI_WS_DISABLE_ORIGIN` 接受更多布尔字面量 | 回退 Go 时统一写 `true`/`false`。 |
| D-006 | 邮件主题 RFC 2047 编码 Q vs B | 解码后语义相同，通常无需处理。 |
| D-007 | 配置数值越界时严格报错 | 修正 `configs` 中的异常数值。 |
| D-008 | CLI 布尔 flag 只接受 `true`/`false` | 把 `--flag=1`/`--flag=TRUE` 改为 `--flag` 或 `--flag=true`。 |
| D-009 | DDNS provider 配置脱敏 | B06 的 GET/list 必须调用 `redact_addition`；写路径保留原值。 |
| D-010 | GeoIP mmdb 初始化只在缺失时下载 | 需要更新时走管理端 `update_database`。 |
| D-011 | 通知 provider 配置脱敏 | 已合并（B07）；B06 的 GET/list 接入 `redact_addition`，写路径保留原值。 |

此外，以下模块的 HTTP 装配尚未完成，不能视为已迁移：

- B04：public API、JSON-RPC、`/ping`；
- B05：agent API 与浏览器 WebSocket（截至 B11 基线未合并）；
- B06：admin API；
- B13：crate 已合并（provider/回调/账户绑定），但 OAuth HTTP 登录/绑定路由仍需
  服务器装配；
- B14：crate 已合并，但静态路由仍需 HTTP server 装配；隐藏节点过滤由 B14/B06 接入。

MJPEG（`/api/mjpeg_live`）已由 B18 补齐，不再属于上述未完成范围；布局、字体
下载/回退、`lang`/`tz_offset` 与 Go 对齐。

## 9. 前端静态资源

### Go 版

Go 版通过 `//go:embed defaultTheme` 把前端打进二进制，构建顺序是：

```sh
cd komari-web
npm install && npm run build
mkdir -p ../komari/public/defaultTheme/dist
cp -r dist/* ../komari/public/defaultTheme/dist/
cp komari-theme.json ../komari/public/defaultTheme/
cp preview.png ../komari/public/defaultTheme/
cd ../komari && go build -o komari
```

运行时还会从 `./data/theme/<name>` 读取自定义主题，并支持
`./data/favicon.ico`。

### Rust 版

`komari-web`（B14）已实现：

- 默认主题静态资源嵌入（`KOMARI_DEFAULT_THEME_DIR` 或占位主题）；
- `data/theme` 自定义主题，缺失文件回退默认主题；
- `/favicon.ico`、`/themes/{id}/{path}`、SPA 回退；
- `custom_head` / `custom_body` / `sitename` / `description` 单次替换；
- 临时分享 token 的 `temp_key` cookie 注入。

但 `komari server` 尚未装配 `routes_for` / `serve_fallback`，所以当前二进制仍不会
通过 HTTP 提供页面。服务器装配前：

- 生产页面继续由 Go 版提供；
- B04/B05/B06 合并前，`/api`、`/ping` 也仍由 Go 提供。不要把这些路径反代到
  尚未实现 HTTP 层的 Rust 二进制；
- 如果只是把前端静态文件独立托管，`/api`、`/ping` 的反代目标仍应是 Go 后端。

`KOMARI_THEME` 只影响启动配置；主题文件由 `komari-web` 按数据库 `theme` 选择，
`KOMARI_DEFAULT_THEME_DIR` 是编译期嵌入路径。

## 10. CLI 命令对照

| Go 命令 | Rust 命令 | 说明 |
| --- | --- | --- |
| `komari server` | `komari server` | 不写子命令也等价于 `server`。 |
| `komari chpasswd -p ...` | `komari chpasswd -p ...` | 写入 Go 兼容 SHA-256 哈希并删除所有会话。 |
| `komari disable2FA` | `komari disable-2fa` / `disable2FA` | 清除 TOTP。 |
| `komari permitPasswordLogin` | `komari permit-login` / `permitPasswordLogin` | 允许密码登录。 |

`chpasswd` 不传用户名时选择 UUID 最小的账户；数据库不存在时只打印
`Database file does not exist.` 并以 0 退出，和 Go 一致。

## 11. 迁移检查清单

- [ ] 已停机并备份 `komari.db`（含 WAL/SHM）与 `data/` 资源。
- [ ] 已确认数据库是 SQLite，schema 兼容性测试通过。
- [ ] 已保留 Go 二进制和原 systemd/Docker 配置。
- [ ] Rust 二进制使用正确的 `KOMARI_VERSION` / `KOMARI_VERSION_HASH` 构建。
- [ ] `WorkingDirectory` 与 `KOMARI_DB_FILE` 指向原数据目录。
- [ ] 端口仍为 `25774`，反向代理的 WebSocket/body/timeout 配置未变。
- [ ] **基线可验证项**：配置解析、`komari-db` schema/compat 测试、CLI 辅助命令。
- [ ] **HTTP 层合并后验证**：`/ping`、登录、agent 上报、管理接口、通知/DDNS。
- [ ] 已阅读 [parity-deviations.md](parity-deviations.md) 并接受 D-001..D-011。
- [ ] 回退步骤已在演练环境验证。
