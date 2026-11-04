# 功能对等（Parity）汇总

本文档汇总 `komari-rs` 当前基线的模块状态、已知差异和迁移建议。
差异明细以 [parity-deviations.md](parity-deviations.md) 为准；本文件只做索引和
状态汇总，不重复修改 D 条目内容。

基线：`main@0382eb3`（B07/B08/B10/B13/B14）。
模块状态**只在本文件维护**，其他文档引用这里。
详细配置见 [CONFIGURATION.md](CONFIGURATION.md)，迁移步骤见
[MIGRATION.md](MIGRATION.md)，部署见 [DEPLOYMENT.md](DEPLOYMENT.md)，路由见
[API.md](API.md)。

## 1. 模块状态

| 模块 | Crate | 状态 | 说明 |
| --- | --- | --- | --- |
| CLI / 启动配置 | `komari-cli`、`komari-config` | **已合并（B01）** | 子命令、flag/env 优先级、日志、版本注入。 |
| 数据库 | `komari-db` | **已合并（B02）** | SQLite schema 兼容、key/value `configs`、仓储层。 |
| 认证 | `komari-auth` | **已合并（B03）** | 密码、会话、TOTP、角色、私有站点中间件。 |
| 公开 API / JSON-RPC | `komari-api` | **待实现（B04）** | 当前只有占位 `lib.rs`。 |
| Agent API / WebSocket | `komari-api` | **待合并（B05）** | 截至 B11 基线（`main@0382eb3`）未合并；实现状态以本表为准。 |
| 管理 API | `komari-api` | **待实现（B06）** | `/api/admin/*` 尚未移植。 |
| 通知 | `komari-notify` | **已合并（B07）** | 7 个原生 provider + empty；Javascript 不支持；provider 脱敏（D-011）已合并。 |
| GeoIP | `komari-geoip` | **已合并（B08）** | mmdb/ip-api/ipinfo/geojs/empty，48h 缓存。 |
| DDNS | `komari-ddns` | **已合并（随 B08 分支）** | cloudflare/webhook/empty、重试、历史。 |
| 记录维护 | `komari-jobs` | **已合并** | 保留期清理、15 分钟压缩、VACUUM/checkpoint。 |
| OAuth / OIDC | `komari-oauth` | **已合并（B13）** | provider/回调/账户绑定已实现；OAuth HTTP 登录/绑定路由待服务器装配。 |
| Web / 静态资源 / 主题 | `komari-web` | **已合并（B14）** | 嵌入默认主题、`/favicon.ico`、`/themes/{id}/{path}`、SPA 回退；需服务器装配 routes/fallback。 |
| 文档 | `docs/` | **本任务（B11）** | 配置、迁移、部署、API、parity。 |

### 当前可运行能力

可以直接使用：

- `komari server` 的配置解析和启动日志（不会监听端口）；
- `komari chpasswd`、`disable-2fa`、`permit-login`；
- `komari-db` 的 schema 迁移和仓储 API；
- `komari-auth` 的密码/会话/TOTP/角色中间件；
- `komari-notify`、`komari-ddns`、`komari-geoip`、`komari-jobs` 的库级 API；
- `komari-web`（B14）的静态资源、主题、SPA 回退和临时分享 cookie 路由（库级 API，
  尚未接入 HTTP server）。

尚不能通过 HTTP 使用：

- `/ping`、`/api/*`、`/api/rpc2`、WebSocket、静态页面；
- OAuth 登录；
- 管理 API 和备份上传/下载。

## 2. D 编号汇总

以下条目来自 [parity-deviations.md](parity-deviations.md)。**D 编号是稳定标识，
后续只追加，不重编号；B11 是唯一编号 owner，其他任务只提交内容。**

| 编号 | 主题 | 类型 | 影响 | 迁移建议 |
| --- | --- | --- | --- | --- |
| D-001 | Javascript 通知 provider 不支持 | 有意差异 | `notification_method=Javascript` 无法发送 | 改用原生 provider 或 webhook。 |
| D-002 | DDNS 删除记录是 Rust 扩展 | 非 Go 扩展 | 仅 cloudflare 支持；empty 报错 | 不依赖该能力；回退 Go 时改用 provider API。 |
| D-003 | 旧库历史迁移（已合并 B20） | 已合并 | 极老库自动迁移 `client_infos` / OAuth / 通知配置 | 备份后直接切换；迁移失败会回滚。 |
| D-004 | GeoIP 缓存上限 10,000 | 有意差异 | 大量唯一 IP 时更早淘汰、增加上游查询 | 通常无需处理；需要时调大常量重建。 |
| D-005 | `KOMARI_WS_DISABLE_ORIGIN` 字面量更宽松 | 有意差异 | `1`/`yes`/`on` 在 Rust 生效、Go 不生效 | 跨版本统一写 `true`/`false`。 |
| D-006 | 邮件主题 Q vs B 编码 | 实现差异（语义等价） | 原始字节不同，解码内容一致 | 通常无需处理。 |
| D-007 | 配置数值越界严格报错 | 兼容性限制 | 越界配置读取失败而非回绕 | 修正 `configs` 中的异常数值。 |
| D-008 | CLI 布尔 flag 只接受 `true`/`false` | 有意差异 | `--flag=1`/`TRUE` 会以退出码 2 失败 | 改为 `--flag` 或 `--flag=true`。 |
| D-009 | DDNS provider 配置脱敏 | 有意安全差异 | 管理 API 不能读回 Cloudflare token/webhook 凭据 | B06 的 GET/list 调用 `redact_addition`；写路径保留原值。 |
| D-010 | GeoIP mmdb 初始化只在缺失时下载 | 实现细节 | 启动不会自动更新本地库 | 需要更新时调用管理端 `update_database`。 |
| D-011 | 通知 provider 配置脱敏 | 有意安全差异（已合并 B07） | 管理 API 不能读回 bot token/密码等 | B06 的 GET/list 接入 `redact_addition`；写路径保留原值。 |

### B07 未占号差异

B07 的 offline 通知不触发 auto-renewal、周期循环由调用方负责这两项**未登记为
D 条目、不占号**；若后续需要正式登记，使用 `D-012` / `D-013`。

### 按影响分类

- **会阻断功能**：D-001（仅 Javascript 通知）。
- **行为差异但通常可接受**：D-002、D-004、D-005、D-006、D-008、D-010。
- **安全/API 契约相关**：D-009、D-011。
- **数据质量相关**：D-007。

### 迁移时的最低检查

1. 通知不使用 `Javascript`（D-001）。
2. DDNS 不依赖删除记录（D-002）。
3. 极老数据库可直接切换；迁移失败会回滚（D-003）。
4. 跨版本脚本使用 `true`/`false` 布尔值（D-005）。
5. 检查 `configs` 数值范围（D-007）。
6. 旧脚本的 `--flag=1`/`--flag=TRUE` 改为 `--flag`/`--flag=true`（D-008）。
7. B06 的管理 API GET/list 必须调用已合并的 DDNS 脱敏（D-009）和通知脱敏
   （D-011）函数；写路径保留原始 `addition`。
8. 需要更新 GeoIP 库时走管理端更新路径（D-010）。

## 3. 未登记为 D 的功能缺口

以下不是“有意行为差异”，而是尚未实现的模块，因此不占用 D 编号：

| 缺口 | 责任 | 影响 |
| --- | --- | --- |
| MJPEG | B04 | `/api/mjpeg_live` 返回明确的未实现响应；其余公开端点已可用。 |

Public/Agent/Admin/RPC/OAuth/静态路由与浏览器 WS 均已由 B04/B05/B06/B09 装配。
生产切换仍建议先按 [MIGRATION.md](MIGRATION.md) 验证；参见
[DEPLOYMENT.md](DEPLOYMENT.md)。

## 4. 兼容性验证

### 数据库

```sh
# 使用生产库副本，避免污染
cp data/komari.db /tmp/komari-compat.db
KOMARI_COMPAT_DB=/tmp/komari-compat.db cargo test -p komari-db --test compat
```

`tests/compat.rs` 会逐表比较列和索引；文件不存在时跳过。全量测试：

```sh
cargo test -p komari-db
```

### 配置

```sh
cargo test -p komari-config
cargo test -p komari-cli
```

### 模块

```sh
cargo test -p komari-auth
cargo test -p komari-notify
cargo test -p komari-geoip
cargo test -p komari-ddns
cargo test -p komari-jobs
```

### 文档

```sh
./docs/check-docs.sh
```

## 5. 已知限制

- 本基线没有 HTTP 运行时，无法做端到端 API/WS 验证。
- 截至 B11 基线（`main@0382eb3`），B05 的 agent API 未合并；本文件不把它算作
  “已合并”。
- MySQL 未实现；Go 的 MySQL 分支也被注释，生产应使用 SQLite。
- `Javascript` 通知 provider 是唯一明确不支持的原生 provider。
- 极老数据库的历史迁移需要 Go 版协助。
- B14 的 `komari-web` 已合并（默认主题嵌入、静态/SPA/主题路由），但仍需服务器
  装配 `routes_for` / `serve_fallback`。
- B13 的 `komari-oauth` 已合并（provider/回调/账户绑定）；OAuth HTTP 登录/绑定
  路由仍需服务器装配。
- 参考库还有 `command_audits`、`ping_records`、`ping_tasks`、`task_results`、
  `tasks` 等表；Rust 不创建/迁移它们，回退时需保留或由 Go 管理。
- B07 已按协议删除独立 parity 文件，只提交代码/测试；D-011 由 B11 统一维护。

## 6. 验收建议

1. 用生产库副本跑 `komari-db` compat 测试。
2. 用 `komari server` 检查配置解析（不会监听端口）。
3. 在演练环境用 Go 版完成历史迁移，再切换 Rust。
4. 逐项确认 D-001..D-011 的迁移建议。
5. B04/B05/B06 合并并完成 B13 OAuth HTTP/B14 静态路由装配后，按 [API.md](API.md)
   的路由清单做端到端回归。
6. 按 [DEPLOYMENT.md](DEPLOYMENT.md) 验证 systemd/Docker、反代 WS、备份和健康检查。

## 5. B09 跨 crate 变更说明

B09 的服务器装配需要少量跨 crate 改动，均为验收项所需的最小改动：

- `komari-auth`：新增 `BufferedBodyLimit` / `SkipBodyBuffering`，让普通请求的
  chunked 超限返回 413，并让 `/api/admin/upload/backup` 跳过 AuthLayer 的整包缓冲，
  保持 B06 的流式上传。
- `komari-config` / `komari-cli`：新增 `KOMARI_BODY_LIMIT` / `--body-limit`，用于
  配置普通请求体上限。
- `komari-notify`：新增 `renewal` 模块，供上线事件和每日 09:00 调度复用。
- `komari-oauth`：仅随 main 的 B04/B06 合并同步，B09 未改变 provider 语义。
- `docs`：更新服务器装配、OAuth、静态资源、浏览器 WS 与 body limit 状态。
