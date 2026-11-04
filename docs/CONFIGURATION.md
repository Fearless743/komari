# Komari Rust 配置参考

本文档描述 `komari-rs` 当前基线（B11，已合并 `main@0382eb3`）实际读取的启动配置、
命令行参数和持久化设置。所有键名、默认值和路径均与代码逐项核对：

- 启动配置：`crates/komari-config/src/config.rs`、`crates/komari-config/src/keys.rs`
- CLI：`crates/komari-cli/src/cli.rs`、`crates/komari-cli/src/lib.rs`
- 数据库：`crates/komari-db/src/db.rs`
- 日志：`crates/komari-cli/src/logging.rs`
- 站点/通知/DDNS/GeoIP 设置：`crates/komari-config/src/site.rs`、
  `crates/komari-db/src/models/`、`crates/komari-ddns/`、`crates/komari-geoip/`、
  `crates/komari-notify/`

> **当前实现边界**
>
> 本基线的 `komari server` 只解析配置并打印启动信息，HTTP 监听、WebSocket、
> CORS 和 cloudflared 启动尚未装配（分别由 B04/B05/B06 及 B13/B14 服务器装配负责）。
> 下文中标注“已解析”的配置项已经进入 `Config`，但其运行时效果要等对应模块合并后
> 才会生效；标注“已实现”的模块可以直接使用。部署现状见
> [DEPLOYMENT.md](DEPLOYMENT.md)，模块状态见 [PARITY.md](PARITY.md)。

## 1. 配置优先级

启动配置按以下顺序解析，**前者覆盖后者**：

1. 命令行 flag
2. 环境变量
3. 内置默认值

与 Go 版一致，**空环境变量等同于未设置**（`KOMARI_DB_FILE=""` 会回退到默认值）。
CLI 的 `--database` 与 `--db-file` 是同一个参数，`--db-file` 是可见别名。

## 2. 环境变量总表

| 环境变量 | 对应 CLI 参数 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `KOMARI_LISTEN` | `--listen`, `-l`（`server` 子命令） | `0.0.0.0:25774` | HTTP 监听地址。端口默认 `25774`。 |
| `KOMARI_DB_TYPE` | `--db-type`, `-t` | `sqlite` | `sqlite` / `sqlite3` / `mysql`；当前只有 SQLite 可用，打开数据库的路径会拒绝 MySQL。 |
| `KOMARI_DB_FILE` | `--database`, `-d` / `--db-file` | `data/komari.db` | SQLite 文件路径。Go 的默认值是 `./data/komari.db`，指向同一位置。 |
| `KOMARI_DB_HOST` | `--db-host` | `localhost` | MySQL/网络数据库主机；SQLite 下不使用。 |
| `KOMARI_DB_PORT` | `--db-port` | `3306` | MySQL/网络数据库端口；按 `u16` 解析。 |
| `KOMARI_DB_USER` | `--db-user` | `root` | MySQL/网络数据库用户名；SQLite 下不使用。 |
| `KOMARI_DB_PASS` | `--db-pass` | 空 | MySQL/网络数据库密码；SQLite 下不使用。 |
| `KOMARI_DB_NAME` | `--db-name` | `komari` | MySQL/网络数据库名；SQLite 下不使用。 |
| `KOMARI_LOG_LEVEL` | `--log-level` | 开发构建 `debug`，否则 `info` | `trace`、`debug`、`info`、`warn`、`error`、`off`；另接受别名 `warning`、`none`。 |
| `KOMARI_THEME` | `--theme` | `default` | 启动配置中的主题名。实际站点主题以数据库 `configs.theme` 为准。 |
| `KOMARI_ALLOW_CORS` | `--allow-cors[=BOOL]` | `false` | 启动配置中的 CORS 开关。运行时 CORS 由持久化 `allow_cors` 控制。 |
| `KOMARI_ENABLE_CLOUDFLARED` | `--enable-cloudflared[=BOOL]` | `false` | 是否启用 cloudflared。当前只解析，尚未启动隧道进程。 |
| `KOMARI_CLOUDFLARED_TOKEN` | 无 | 未设置 | cloudflared tunnel token。当前只解析，尚未使用。 |
| `KOMARI_WS_DISABLE_ORIGIN` | `--ws-disable-origin[=BOOL]` | `false` | 是否关闭 WebSocket Origin 校验。当前只解析，WS 由 B05/B14 装配。 |
| `KOMARI_BODY_LIMIT` | `--body-limit <BYTES>` | `16777216`（16 MiB） | 普通 HTTP 请求体上限（字节），必须为正整数。`/api/admin/upload/backup` 使用独立的 2 GiB 上限。 |

### 编译期与测试变量

以下不是运行时启动配置，但同样以 `KOMARI_` 开头：

| 变量 | 阶段 | 说明 |
| --- | --- | --- |
| `KOMARI_VERSION` | 编译期 | 覆盖 `CARGO_PKG_VERSION`，见 `crates/komari-cli/src/version.rs`。 |
| `KOMARI_VERSION_HASH` | 编译期 | 注入 commit hash；缺省 `unknown`，会启用 `debug` 默认日志。 |
| `KOMARI_COMPAT_DB` | 测试 | `komari-db` 的 compat 测试用它指定对照数据库；不是服务配置。 |

### 构建/编译期变量（不由 komari-config 读取）

以下变量由 B10 构建镜像或 B14 构建脚本读取，**不会进入 `komari-config` 的
`Config`**：

| 变量 | 阶段/读取方 | 说明 |
| --- | --- | --- |
| `KOMARI_THEME_SRC` | B10 Dockerfile 构建参数 | 主题源目录（相对构建上下文），默认 `crates/komari-web/defaultTheme`，复制到镜像 `/src/theme`。 |
| `KOMARI_DEFAULT_THEME_DIR` | B14 编译期（`crates/komari-web/build.rs`） | 默认主题目录；Docker 默认 `/src/theme`，未设置时嵌入 `crates/komari-web/defaultTheme` 占位主题。不是 compose 运行时变量。 |

> 旧的 `KOMARI_WEB_DIST` / `KOMARI_WEB_DIR` 已废弃，当前 B10 Dockerfile 不再读取。

### 布尔值解析

- 环境变量：`1` / `true` / `yes` / `on` 为真，`0` / `false` / `no` / `off` 为假；
  大小写不敏感，前后空白会被忽略。
- CLI flag：clap 的 `BoolValueParser` **只接受小写 `true` / `false`**；
  `--flag` 等价于 `--flag=true`。`--flag=1`、`--flag=TRUE`、`--flag=yes` 会报
  `invalid value` 并以退出码 `2` 退出。
- 带值写法必须使用等号：`--allow-cors=false`。`--allow-cors false` 会被当作
  未知的位置参数。
- 该差异已登记为 [parity-deviations.md](parity-deviations.md) D-008；旧脚本请把
  `--flag=1` 改为 `--flag` 或 `--flag=true`。

### 数值解析

- `KOMARI_DB_PORT` / `--db-port` 按 `u16` 解析，超出 `0..=65535` 会报配置错误。
- `--db-port` 由 clap 直接按 `u16` 解析；`KOMARI_DB_PORT` 由配置层解析。
- `KOMARI_LOG_LEVEL` 大小写不敏感；非法值会导致启动失败，而不是静默回退。

## 3. CLI 参数

### 全局参数（所有子命令可用）

| 参数 | 取值 | 说明 |
| --- | --- | --- |
| `-t`, `--db-type <DB_TYPE>` | `sqlite` / `sqlite3` / `mysql` | 数据库类型。 |
| `-d`, `--database <FILE>` | 路径 | SQLite 文件路径；别名 `--db-file`。 |
| `--db-host <HOST>` | 字符串 | MySQL/网络数据库主机。 |
| `--db-port <PORT>` | `u16` | MySQL/网络数据库端口。 |
| `--db-user <USER>` | 字符串 | MySQL/网络数据库用户名。 |
| `--db-pass <PASSWORD>` | 字符串 | MySQL/网络数据库密码。 |
| `--db-name <NAME>` | 字符串 | MySQL/网络数据库名。 |
| `--log-level <LEVEL>` | `trace`/`debug`/`info`/`warn`/`error`/`off` | 日志级别。 |
| `--theme <THEME>` | 字符串 | 启动主题名。 |
| `--allow-cors[=<BOOL>]` | 布尔 | 启动 CORS 开关。 |
| `--enable-cloudflared[=<BOOL>]` | 布尔 | 启用 cloudflared。 |
| `--ws-disable-origin[=<BOOL>]` | 布尔 | 关闭 WS Origin 校验。 |
| `--body-limit <BYTES>` | 正整数 | 普通 HTTP 请求体上限；备份上传路径使用独立上限。 |

全局参数可以写在子命令之前或之后（clap `global = true`）。

### 子命令

| 命令 | 别名 | 参数 | 行为 |
| --- | --- | --- | --- |
| `komari server` | 无 | `-l`, `--listen <ADDR>` | 启动服务器。不写子命令时等价于 `server`。 |
| `komari chpasswd` | 无 | `-u`, `--username <USERNAME>`；`-p`, `--password <PASSWORD>` | 写入 Go 兼容的 SHA-256 密码哈希并删除所有登录会话。 |
| `komari disable-2fa` | `disable2FA` | `-u`, `--username <USERNAME>` | 清除指定用户的 TOTP secret；不传用户名时清除所有用户。 |
| `komari permit-login` | `permitPasswordLogin` | 无 | 将 `disable_password_login` 设为 `false`。 |

### 退出码与特殊行为

| 场景 | 退出码 | 输出/行为 |
| --- | --- | --- |
| 命令成功 | `0` | 正常输出。 |
| 运行时错误（数据库打开失败、配置非法等） | `1` | stderr 输出 `Error: ...`。 |
| clap 参数错误（未知参数、非法取值） | `2` | clap 打印 usage/错误。 |
| `chpasswd` 未提供 `--password` | `0` | 打印 `chpasswd` 帮助，不修改数据库。 |
| `chpasswd` 指向不存在的数据库文件 | `0` | 打印 `Database file does not exist.`，不创建数据库。 |
| `disable-2fa` / `permit-login` 指向不存在的数据库 | `0` | 创建并迁移数据库后执行。 |
| 辅助命令使用非 SQLite 数据库 | `1` | 报 `only sqlite is implemented`。 |

`chpasswd` 默认选择 UUID 最小的账户，与 Go 命令一致。密码哈希格式为
`base64(SHA-256(password + "06Wm4Jv1Hkxx"))`，Go 回退版本仍可认证。

## 4. 数据库配置

### SQLite（当前唯一可用）

| 项目 | 值 |
| --- | --- |
| 默认文件 | `data/komari.db`（Go：`./data/komari.db`） |
| 日志模式 | WAL（读写句柄） |
| busy timeout | 5 秒 |
| 外键约束 | 关闭（与 Go 的 SQLite 默认行为一致） |
| 父目录 | 打开时自动创建 |
| 内存库 | `:memory:` 可用于测试/临时运行 |

SQLite 连接由 `crates/komari-db/src/db.rs` 的 `DbOptions` 控制；CLI 只暴露文件路径。
同一 SQLite 文件可被 Go 和 Rust 交替打开，schema 迁移是幂等的
`CREATE TABLE/INDEX IF NOT EXISTS` 加缺失列补齐，不会删除已有数据。细节见
[MIGRATION.md](MIGRATION.md)。

### MySQL

`KOMARI_DB_TYPE=mysql` 会被配置层接受，但当前没有 MySQL 实现：

- `Db::open_default()` 返回 `UnsupportedDatabase`；
- `chpasswd` / `disable-2fa` / `permit-login` 直接报错；
- Go 版的 MySQL 分支同样处于注释状态，因此不要依赖 MySQL 部署。

## 5. 日志

- 输出到 **stderr**，格式为 tracing 默认格式；不是终端时关闭 ANSI 颜色。
- 不读取 `RUST_LOG`；级别完全由 `--log-level` / `KOMARI_LOG_LEVEL` /
  默认值决定。
- 默认级别：`KOMARI_VERSION_HASH` 为 `unknown`（开发构建）时 `debug`，否则
  `info`。这与 Go 的 `utils.VersionHash` 行为一致。
- `off` 会关闭所有日志；`trace` 最详细。
- 启动时会打印版本、hash、监听地址、数据库类型/文件、主题、CORS、日志级别、
  cloudflared 和 WS Origin 配置。

## 6. 主题、CORS、WebSocket 与 cloudflared

### 主题

- `KOMARI_THEME` / `--theme` 只影响启动配置和启动日志，默认 `default`。
- 站点实际主题读取数据库 `configs` 表的 `theme` 键（默认 `default`）。
- Go 版从 `public/defaultTheme` 嵌入前端，并从 `data/theme/<name>` 读取自定义主题；
  Rust 的 `komari-web`（B14）已实现默认主题嵌入、`/themes/{id}/{path}`、
  `/favicon.ico` 和 SPA 回退；服务器装配 `routes_for` / `serve_fallback` 后才会
  通过 HTTP 提供页面。

### CORS

- 启动配置：`KOMARI_ALLOW_CORS` / `--allow-cors`，默认 `false`。
- 运行时配置：数据库 `configs.allow_cors`，默认 `false`。
- Go 版在每次请求时读取持久化值并动态设置 CORS 头；Rust 的 HTTP 层尚未装配，
  B04/B06 接入时应以持久化 `allow_cors` 为准。

### WebSocket Origin

- 启动配置：`KOMARI_WS_DISABLE_ORIGIN` / `--ws-disable-origin`，默认 `false`。
- 预期行为：为 `true` 时跳过 Origin 校验；否则要求 `Origin` 的 host 与请求
  `Host` 一致。浏览器 WebSocket 还会在 `allow_cors=true` 时直接放行。
- 当前只有配置解析；具体升级逻辑由 B05（agent/browser WS）和 B14（页面）装配。

### cloudflared

- `KOMARI_ENABLE_CLOUDFLARED` / `--enable-cloudflared` 默认 `false`。
- `KOMARI_CLOUDFLARED_TOKEN` 没有对应 CLI 参数。
- 当前 Rust 只把这两个值读入 `Config`，没有下载/启动 `cloudflared` 的代码；
  需要隧道时请继续使用 Go 版或外部 systemd 服务。

## 7. 持久化站点设置（`configs` 表）

当前 schema 使用 key/value 形式的 `configs` 表（Komari >= 1.1.4）。`value` 列存
**JSON 文本**，例如字符串 `"Komari"`、布尔 `true`、数字 `80`、模板字符串。
`SiteConfig` 的字段名与 Go `config.Legacy` 的 JSON tag 一致。

| 键 | 类型 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `sitename` | string | `"Komari"` | 站点名称。 |
| `description` | string | `"A simple server monitor tool."` | 站点描述。 |
| `allow_cors` | bool | `false` | 运行时 CORS 开关。 |
| `theme` | string | `"default"` | 当前主题目录名。 |
| `private_site` | bool | `false` | 私有站点；未登录访问 `/api/*` 返回 401。 |
| `api_key` | string | `""` | 管理员 API key；长度至少 12 才生效。 |
| `auto_discovery_key` | string | `""` | agent 自动发现注册 key。 |
| `script_domain` | string | `""` | 自定义脚本域名。 |
| `send_ip_addr_to_guest` | bool | `false` | 是否向访客发送 IP 地址。 |
| `eula_accepted` | bool | `false` | EULA 接受标记。 |
| `base_scripts_url` | string | `""` | 前端脚本基础 URL。 |
| `geo_ip_enabled` | bool | `true` | GeoIP 总开关。 |
| `geo_ip_provider` | string | `"ipinfo"` | GeoIP provider，见第 8 节。 |
| `ddns_enabled` | bool | `false` | DDNS 总开关。 |
| `ddns_provider` | string | `"none"` | DDNS provider，见第 9 节。 |
| `ddns_sync_interval` | int | `10` | DDNS 同步间隔，单位分钟。Go 调度器对 `<= 0` 回退到 10；Rust 的周期调度尚未装配。 |
| `o_auth_enabled` | bool | `false` | OAuth 登录开关。 |
| `o_auth_provider` | string | `"github"` | OAuth provider，见第 10 节。 |
| `disable_password_login` | bool | `false` | 禁用密码登录；启用前必须已有 SSO 绑定账户。 |
| `custom_head` | string | `""` | 注入到 `</head>` 前的 HTML。 |
| `custom_body` | string | `""` | 注入到 `</body>` 前的 HTML。 |
| `notification_enabled` | bool | `true` | 通知总开关。 |
| `notification_method` | string | `"none"` | 通知 provider，见第 11 节。 |
| `notification_template` | string | 见下 | 通知文本模板。 |
| `expire_notification_enabled` | bool | `true` | 到期通知开关。 |
| `expire_notification_lead_days` | int | `7` | 到期前多少天开始通知。 |
| `login_notification` | bool | `true` | 登录通知开关。 |
| `traffic_limit_percentage` | float | `80.0` | 流量提醒阈值百分比。 |
| `record_enabled` | bool | `true` | 是否记录监控数据，详见第 12 节。 |
| `record_preserve_time` | int | `720` | 记录保留小时数，默认 30 天。 |
| `updated_at` | timestamp | 无 | 旧版配置保留键；Rust `SiteConfig` 不读写。 |

默认通知模板（`DEFAULT_NOTIFICATION_TEMPLATE`）：

```text
{{emoji}}{{emoji}}{{emoji}}
Event: {{event}}
Clients: {{client}}
Message: {{message}}
Time: {{time}}
```

### 认证与安全相关键

以下键不属于 `SiteConfig`，但认证层会读取：

| 键 | 类型 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `api_key` | string | `""` | `Authorization: Bearer <key>` 的管理员 key；少于 12 字符不生效。 |
| `private_site` | bool | `false` | 私有站点开关。 |
| `tempory_share_token` | string | 无 | 临时分享 token（注意拼写就是 `tempory`）。 |
| `tempory_share_token_expire_at` | int | 无 | 临时分享过期时间，Unix 秒。 |
| `disable_password_login` | bool | `false` | 禁用密码登录。 |

认证顺序（`komari-auth` 已实现）：

1. `Authorization: Bearer <api_key>` → 管理员；
2. `session_token` cookie → 管理员；
3. `?token=<agent token>`，或非 GET 请求 JSON body 中的 `token` → agent/client；
4. 其他 → 访客。

会话默认有效期 30 天（`2_592_000` 秒），cookie 名为 `session_token`。
私有站点模式下，未登录访问受保护 `/api/*` 返回
`401 {"status":"error","message":"Private site is enabled, please login first."}`。

## 8. GeoIP

| 键 | 值 |
| --- | --- |
| `geo_ip_enabled` | `true` / `false`，默认 `true` |
| `geo_ip_provider` | `empty`、`mmdb`、`ip-api`、`ipinfo`、`geojs`，默认 `ipinfo` |

- `mmdb` 使用本地 MaxMind 数据库：
  - 路径：`./data/GeoLite2-Country.mmdb`
  - 下载地址：`https://raw.githubusercontent.com/Loyalsoldier/geoip/release/GeoLite2-Country.mmdb`
  - 缺失时首次初始化会尝试下载；父目录会自动创建。
- `ip-api` 使用 `http://ip-api.com/json/<ip>`；`ipinfo` 使用
  `https://ipinfo.io/<ip>/json`；`geojs` 使用
  `https://get.geojs.io/v1/ip/geo/<ip>.json`。
- 查询结果缓存 48 小时，缓存容量上限 10,000 条（Rust 扩展，见
  [parity-deviations.md](parity-deviations.md) D-004）。
- provider 初始化失败或 `geo_ip_enabled=false` 时回退到 empty provider，
  查询返回空 `GeoInfo`，不会 panic。
- GeoIP 没有 `KOMARI_*` 环境变量，全部通过数据库配置。

## 9. DDNS

| 键 | 值 |
| --- | --- |
| `ddns_enabled` | `true` / `false`，默认 `false` |
| `ddns_provider` | `none`、`cloudflare`、`webhook`、`empty`，默认 `none` |
| `ddns_sync_interval` | 分钟，默认 `10`；Go 调度器对 `<= 0` 回退到 10，Rust 周期调度待装配 |

provider 配置存放在 `ddns_providers` 表：

| `name` | `addition` JSON 字段 |
| --- | --- |
| `cloudflare` | `api_token`（必填）、`zone_id`（必填）、`record_id`（可选全局回退）、`record_type`（`A`/`AAAA`，默认 `A`）、`ttl`（默认 `1`，Cloudflare 自动）、`proxied`（默认 `false`） |
| `webhook` | `url`（必填）、`method`（`POST`/`PUT`/`PATCH`，默认 `POST`）、`content_type`（默认 `application/json`）、`headers`（JSON 字符串）、`body`（模板）、`response_json_path`（从响应提取 record id，例如 `data.record_id`） |
| `empty` | `{}`；调用会失败并提示使用有效 provider |

> `cloudflare` 的 `record_type` / `ttl` / `proxied` 默认值来自配置项元数据
> （UI 建议）；新建 provider 行时序列化的是零值。`record_type` 为空时 provider
> 回退到 `A`，`ttl` 需要显式设置（`1` 表示 Cloudflare 自动）。
> `webhook` 的 `method` 为空时回退 `POST`，`content_type` 为空时回退
> `application/json`。

客户端级字段（`clients` 表）：

| 字段 | 说明 |
| --- | --- |
| `ddns_enabled` | 该节点是否参与 DDNS。 |
| `ddns_hostname` | 主机名，会覆盖 provider 配置中的 `hostname`。 |
| `ddns_record_id` | 记录 ID，会覆盖 provider 配置中的 `record_id`。 |
| `ddns_record_type` | `A` / `AAAA` / `inherit`；`inherit` 使用 provider 配置。 |

同步失败最多重试 3 次，基础退避 2 秒（指数退避）；失败会写入
`ddns_sync_history`。Rust 额外提供 `delete_record`，这不是 Go 接口的一部分，
见 [parity-deviations.md](parity-deviations.md) D-002。

## 10. OAuth / OIDC

| 键 | 值 |
| --- | --- |
| `o_auth_enabled` | `true` / `false`，默认 `false` |
| `o_auth_provider` | `github`、`generic`、`CloudflareAccess`、`qq`，默认 `github` |
| `disable_password_login` | `true` / `false`，默认 `false` |

provider 配置存放在 `oidc_providers` 表（`name` + `addition` JSON）：

| `name` | `addition` 字段 |
| --- | --- |
| `github` | `client_id`、`client_secret` |
| `generic` | `client_id`、`client_secret`、`auth_url`、`token_url`、`user_info_url`、`scope`、`user_id_field` |
| `CloudflareAccess` | `team_domain`、`policy_aud` |
| `qq` | `aggregation_url`（默认 `https://login.qjqq.cn`）、`app_id`、`app_key`、`login_type` |

`komari-oauth`（B13）已实现 provider 加载、授权回调、账户绑定和 state/PKCE；
上述 schema 与 Go 版兼容。OAuth 的 HTTP 登录/绑定端点仍需服务器装配。
`o_auth_provider` 为空或 `none` 时按 `github` 处理，与 Go 一致。

## 11. 通知

| 键 | 值 |
| --- | --- |
| `notification_enabled` | `true` / `false`，默认 `true` |
| `notification_method` | `none`、`telegram`、`bark`、`webhook`、`email`、`Server酱³`、`Server酱Turbo`、`empty`、`Javascript`；默认 `none` |
| `notification_template` | 文本模板，支持 `{{emoji}}`、`{{event}}`、`{{client}}`、`{{message}}`、`{{time}}` |
| `expire_notification_enabled` | 默认 `true` |
| `expire_notification_lead_days` | 默认 `7` |
| `login_notification` | 默认 `true` |
| `traffic_limit_percentage` | 默认 `80.0` |

provider 配置存放在 `message_sender_providers` 表（`name` + `addition` JSON）：

| `name` | `addition` 字段 |
| --- | --- |
| `telegram` | `bot_token`、`chat_id`、`message_thread_id`、`endpoint`（默认 `https://api.telegram.org/bot`） |
| `bark` | `server_url`（默认 `https://api.day.app`）、`device_key`、`icon`、`level`（默认 `timeSensitive`） |
| `webhook` | `url`、`method`（默认 `GET`）、`content_type`（默认 `application/json`）、`headers`、`body`（默认 `{"message":"{{message}}","title":"{{title}}"}`）、`username`、`password` |
| `email` | `host`、`port`（默认 `587`）、`username`、`password`、`sender`、`receiver`、`use_ssl`（默认 `false`）、`use_login_auth`（默认 `false`） |
| `Server酱³` | `api_url`、`tags`（竖线分隔） |
| `Server酱Turbo` | `api_url`、`channel`（竖线分隔）、`noip`、`openid` |
| `empty` | `{}`；发送总是失败 |
| `Javascript` | `script`；通过嵌入式 `boa_engine` 执行脚本，见下文 |

说明：

- provider 的 `addition` 默认值是 Go 的零值结构；UI 默认值（如 Telegram endpoint、
  Bark server/level、Email 587）只通过 `ConfigField::default` 暴露，不会自动写进
  已存 JSON。
- `notification_method` 为空或 `none` 时使用 `empty` provider；provider 配置缺失
  也会回退到 `empty`。
- `Javascript` provider 使用 `boa_engine`（纯 Rust ECMAScript）执行用户脚本，
  与 Go 的 `goja` 语义对齐：每个 provider 一个常驻运行时，脚本只在初始化时
  执行一次，之后复用同一 realm。脚本必须定义 `sendMessage(message, title)`
  （可返回 boolean 或 Promise），可选定义 `sendEvent(event)`；没有 `sendEvent`
  时回退到 `sendMessage` 的文本格式。注入的 API 为 `fetch`、
  `XMLHttpRequest`、`console.log`/`console.error`、`setTimeout` 和 `require`
  （任何模块都抛 `Invalid module`，与 Go 未注册原生模块一致）。
  单次调用限时 30 秒（脚本加载不计时）；错误只回显错误类型（以及失败的入口
  函数），不回显脚本内容或错误消息。已知限制见
  [parity-deviations.md](parity-deviations.md) D-001。

通知规则表：

- `offline_notifications`：`client`、`enable`、`grace_period`（默认 180 秒）、
  `last_notified`。
- `load_notifications`：`name`、`clients`（JSON 数组）、`metric`（默认 `cpu`）、
  `threshold`（默认 `80`）、`ratio`（默认 `0.8`）、`interval`（默认 `15` 分钟）、
  `last_notified`。

## 12. 记录与维护

| 键 | 默认值 | 说明 |
| --- | --- | --- |
| `record_enabled` | `true` | 是否记录监控数据。Go 版关闭后会清空记录；Rust 的清空调度待服务器装配，`komari-jobs` 只负责保留期清理和压缩。 |
| `record_preserve_time` | `720` | 记录保留小时数；`komari-jobs` 清理和压缩使用。 |

`komari-jobs` 已实现：

- 按 `record_preserve_time` 分批删除 `records`、`records_long_term`、
  `gpu_records`、`gpu_records_long_term`；
- 4 小时前的实时数据按 15 分钟桶压缩到长期表；网络指标用 20 分位，其余用 70 分位；
- 先 `VACUUM` 再 `PRAGMA wal_checkpoint(TRUNCATE)`，失败只记日志；
- `spawn()` 不会自动启动，服务器装配时负责调度。

## 13. 配置示例

### 环境变量

```sh
export KOMARI_LISTEN=0.0.0.0:25774
export KOMARI_DB_TYPE=sqlite
export KOMARI_DB_FILE=/var/lib/komari/komari.db
export KOMARI_LOG_LEVEL=info
export KOMARI_ALLOW_CORS=false
export KOMARI_WS_DISABLE_ORIGIN=false
```

### 命令行

```sh
komari server \
  --listen 0.0.0.0:25774 \
  --db-type sqlite \
  --database /var/lib/komari/komari.db \
  --log-level info
```

### 只读检查配置

当前 `server` 子命令不会监听端口，可以用它检查解析结果：

```sh
komari server --listen 127.0.0.1:25774 --database /tmp/komari.db --log-level debug
```

stderr 会打印 `startup configuration`，包含最终生效的监听地址、数据库文件、
主题、CORS、日志级别等。

## 14. 自检

仓库提供 `docs/check-docs.sh`，会核对：

- 文档中出现的 `KOMARI_*` 环境变量是否存在于代码；
- CLI flag、子命令是否存在于 `crates/komari-cli/src/cli.rs`；
- 持久化配置键是否存在于 `crates/komari-config/src/keys.rs`；
- 文档中的相对链接是否存在。

```sh
./docs/check-docs.sh
```
