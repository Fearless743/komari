# Rust API 路由概览

本文档列出 Komari Go 版的现有路由，以及它们在 `komari-rs` 当前基线中的实现状态。
**只有标注“已实现”的路由才由 Rust 代码提供**；其余是 Go 参考路由或后续任务范围，
不要据此认为 Rust 已经提供对应端点。

基线：`main@0382eb3`（B07/B08/B10/B13/B14）。
**模块状态只在 [PARITY.md](PARITY.md) 维护**，本文只描述路由范围和 Go 参考。

## 1. 当前状态

- `crates/komari-api/src/lib.rs` 目前只有占位注释，没有 `Router`、handler 或
  HTTP server。
- `crates/komari-oauth`（B13）已实现 provider/回调/账户绑定；OAuth HTTP 登录/
  绑定端点尚未装配。
- `crates/komari-web`（B14）已实现静态资源/主题/SPA 路由，但 `komari server`
  尚未合并这些路由，当前不会监听端口。
- 截至 B11 基线（`main@0382eb3`），B05 的 agent API 和 WebSocket **尚未合并**；
  状态以 [PARITY.md](PARITY.md) 为准，本文不重复分支状态。

因此当前基线**没有可调用的 Rust HTTP 端点**。部署时应继续使用 Go 版，直到
B04/B05/B06 合并并完成 B13 OAuth HTTP/B14 静态路由装配。

## 2. 认证模型（B03 已实现）

`komari-auth` 已实现与 Go `api.IdentityMiddleware` 对齐的身份解析：

| 顺序 | 凭据 | 角色 |
| --- | --- | --- |
| 1 | `Authorization: Bearer <api_key>` | `Admin` |
| 2 | `session_token` cookie | `Admin` |
| 3 | `?token=<agent token>`，或非 GET 请求 JSON body 的 `token` | `Client` |
| 4 | 无凭据 | `Guest` |

规则：

- `Authorization: Bearer` 只用于管理员 API key（`configs.api_key`，至少 12 字符），
  不会被当作 agent token。
- agent 的 `?token=` / body `token` 只用于 `/api/clients/*`。
- `/api/clients/*` 接受 `Admin` 或 `Client`；`/api/admin/*` 只接受 `Admin`。
- 私有站点模式（`private_site=true`）下，未登录访问受保护 `/api/*` 返回
  `401 {"status":"error","message":"Private site is enabled, please login first."}`。
- 角色不足返回 `401 {"status":"error","message":"Unauthorized."}`。
- 配置读取失败返回 `500 {"status":"error","message":"Failed to get configuration."}`。

公开路径前缀（`komari-auth::PUBLIC_PATH_PREFIXES`）：`/ping`、`/api/public`、
`/api/login`、`/api/me`、`/api/oauth`、`/api/oauth_callback`、`/api/version`、
`/api/recent`、`/api/admin`、`/api/clients/`。其中 `/api/admin` 和
`/api/clients/` 仍由角色中间件保护。

## 3. 路由状态总表

| 路由组 | 代表端点 | Rust 状态 | 责任 |
| --- | --- | --- | --- |
| 健康检查 | `GET /ping`（Go 用 `Any`） | 已实现（B09；提供 public router 时由 B04 接管） | B09/B04 |
| 公开 API | `/api/login`、`/api/public`、`/api/nodes`、`/api/version` 等 | 未实现 | B04 |
| Agent API | `/api/clients/register`、`/api/clients/report`、`/api/clients/uploadBasicInfo` | 见 PARITY | B05 |
| 浏览器 WebSocket | `GET /api/clients` | 已实现（B05/B09） | B05/B09 |
| 管理 API | `/api/admin/*` | 未实现 | B06 |
| JSON-RPC | `GET/POST /api/rpc2` | 未实现 | B04 |
| OAuth | `/api/oauth`、`/api/oauth_callback` | 已实现（B09 装配） | B09 |
| 静态资源 | `/`、`/themes/:id/*path`、`/favicon.ico` | 已实现（komari-web + B09 装配） | B09 |
| MJPEG | `/api/mjpeg_live` | 未实现 | B04 |

## 4. 公开路由（Go 参考，Rust 状态）

> B09 已装配 `/ping`、`/api/oauth*`、浏览器 WS 与静态资源。提供 B04 的
> `public_router` 时，`/ping` 和 OAuth 路由由 B04 接管以避免重复路由；具体状态以
> [PARITY.md](PARITY.md) 为准。

以下端点来自 `komari/cmd/server.go`：

| 方法 | 路径 | 说明 | Rust 责任 |
| --- | --- | --- | --- |
| `ANY` | `/ping` | 返回纯文本 `pong`。 | B04 |
| `POST` | `/api/login` | 密码登录，设置 `session_token`。 | B04 |
| `GET` | `/api/me` | 当前身份信息。 | B04 |
| `GET` | `/api/clients` | 浏览器实时数据 WebSocket（不是普通 JSON）。 | B05（见 PARITY） |
| `GET` | `/api/nodes` | 节点列表/公开信息。 | B04 |
| `GET` | `/api/public` | 公开站点设置。 | B04 |
| `GET` | `/api/oauth` | 发起 OAuth 登录。 | B13 |
| `GET` | `/api/oauth_callback` | OAuth 回调。 | B13 |
| `GET` | `/api/logout` | 注销并清除会话。 | B04 |
| `GET` | `/api/version` | 服务端版本。 | B04 |
| `GET` | `/api/recent/:uuid` | 指定节点最近记录。 | B04 |
| `GET` | `/api/records/load` | 按 UUID/时间范围读取记录。 | B04 |
| `GET` | `/api/mjpeg_live` | MJPEG 实时流。 | B04 |

`/api/clients` 是 WebSocket：客户端发送 `get` 或 `get <uuid>`，服务端返回
`{"status":"success","data":{"online":[...],"data":{...}}}`。未登录时过滤
`hidden` 节点、private-site 校验与 `session_token` 独立校验由 B09 实现。

## 5. Agent 路由

### 5.1 本基线

`komari-api` 是占位 crate，以下端点在本基线**不存在**。

### 5.2 B05 的实现范围

B05 计划覆盖以下路由；是否已合并、有哪些边界，统一以
[PARITY.md](PARITY.md) 和 B05 分支文档为准：

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `POST` | `/api/clients/register` | 自动发现注册；使用 `auto_discovery_key`。 |
| `GET` | `/api/clients/report` | agent 持久 WebSocket 上报。 |
| `POST` | `/api/clients/report` | 一次性 HTTP 上报。 |
| `POST` | `/api/clients/uploadBasicInfo` | 静态硬件/OS/网络信息上报。 |
| `GET` | `/api/clients` | 浏览器 WebSocket 实时数据。 |

## 6. 管理路由（Go 参考，Rust 待 B06）

以下路由均来自 Go `cmd/server.go`，需要 `Admin` 角色。Rust 当前未实现，列在这里
是为了明确 B06 的范围，不代表 Rust 已有这些端点。

### 备份与测试

| 方法 | 路径 |
| --- | --- |
| `GET` | `/api/admin/download/backup` |
| `POST` | `/api/admin/upload/backup` |
| `GET` | `/api/admin/test/geoip` |
| `POST` | `/api/admin/test/sendMessage` |

### 更新

| 方法 | 路径 |
| --- | --- |
| `POST` | `/api/admin/update/mmdb` |
| `POST` | `/api/admin/update/user` |
| `PUT` | `/api/admin/update/favicon` |
| `POST` | `/api/admin/update/favicon` |
| `POST` | `/api/admin/update/ddns` |

### 设置

| 方法 | 路径 |
| --- | --- |
| `GET` / `POST` | `/api/admin/settings/` |
| `GET` / `POST` | `/api/admin/settings/oidc` |
| `GET` / `POST` | `/api/admin/settings/message-sender` |
| `GET` / `POST` | `/api/admin/settings/ddns` |
| `POST` | `/api/admin/settings/ddns/cloudflare/zones` |
| `GET` / `DELETE` | `/api/admin/settings/ddns/history` |

### 主题

| 方法 | 路径 |
| --- | --- |
| `PUT` | `/api/admin/theme/upload` |
| `GET` | `/api/admin/theme/list` |
| `POST` | `/api/admin/theme/delete` |
| `GET` | `/api/admin/theme/set` |
| `POST` | `/api/admin/theme/update` |
| `POST` | `/api/admin/theme/import` |
| `POST` | `/api/admin/theme/settings` |

### 节点、记录、会话、2FA

| 方法 | 路径 |
| --- | --- |
| `POST` | `/api/admin/client/add` |
| `GET` | `/api/admin/client/list` |
| `GET` | `/api/admin/client/:uuid` |
| `POST` | `/api/admin/client/:uuid/edit` |
| `POST` | `/api/admin/client/:uuid/remove` |
| `GET` | `/api/admin/client/:uuid/token` |
| `POST` | `/api/admin/client/order` |
| `POST` | `/api/admin/record/clear` |
| `POST` | `/api/admin/record/clear/all` |
| `GET` | `/api/admin/oauth2/bind` |
| `POST` | `/api/admin/oauth2/unbind` |
| `GET` | `/api/admin/session/get` |
| `POST` | `/api/admin/session/remove` |
| `POST` | `/api/admin/session/remove/all` |
| `GET` | `/api/admin/2fa/generate` |
| `POST` | `/api/admin/2fa/enable` |
| `POST` | `/api/admin/2fa/disable` |
| `GET` | `/api/admin/logs` |

### 剪贴板

| 方法 | 路径 |
| --- | --- |
| `GET` | `/api/admin/clipboard/:id` |
| `GET` | `/api/admin/clipboard` |
| `POST` | `/api/admin/clipboard` |
| `POST` | `/api/admin/clipboard/:id` |
| `POST` | `/api/admin/clipboard/remove` |
| `POST` | `/api/admin/clipboard/:id/remove` |

### 通知

| 方法 | 路径 |
| --- | --- |
| `GET` | `/api/admin/notification/offline` |
| `POST` | `/api/admin/notification/offline/edit` |
| `POST` | `/api/admin/notification/offline/enable` |
| `POST` | `/api/admin/notification/offline/disable` |
| `GET` | `/api/admin/notification/load/` |
| `POST` | `/api/admin/notification/load/add` |
| `POST` | `/api/admin/notification/load/delete` |
| `POST` | `/api/admin/notification/load/edit` |

## 7. JSON-RPC

| 方法 | 路径 | Rust 状态 |
| --- | --- | --- |
| `GET` / `POST` | `/api/rpc2` | 未实现，B04 |

Go 的 JSON-RPC 方法分发在 `api/jsonRpc/`。Rust 的 `komari-api` 目前没有 RPC
分发器；不要在客户端依赖任何 Rust RPC 方法，直到 B04 合并。

## 8. 静态资源与 SPA

Go 版由 `public.Static` 提供：

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/favicon.ico` | 优先 `data/favicon.ico`，其次当前主题 `dist/favicon.ico`。 |
| `GET` | `/themes/:id/*path` | 自定义主题文件。 |
| `GET` | 其他未匹配路径 | SPA 回退到 `dist/index.html`，并注入站点名/描述/custom head/body。 |

Rust 的 `komari-web`（B14）已实现这些路由：`routes` / `routes_for` 提供
`/favicon.ico` 与 `/themes/{id}/{path}`，`serve_fallback` 提供 SPA/静态回退。
服务器装配时需把 `routes_for` 合并进主 Router，并把 `serve_fallback` 作为 fallback；
在 B04/B05/B06 的 HTTP server 合并前，二进制仍不会提供页面或主题资源。

## 9. 响应与错误约定

已实现的 `komari-auth` 使用 Go 兼容错误体：

```json
{"status":"error","message":"Unauthorized."}
```

```json
{"status":"error","message":"Private site is enabled, please login first."}
```

```json
{"status":"error","message":"Failed to get configuration."}
```

成功响应由各 handler 决定（Go 版常见 `{"status":"success", ...}`）。当前 Rust
没有 handler，因此没有可依赖的成功响应格式。

## 10. 责任边界

| 任务 | 范围 | 当前状态 |
| --- | --- | --- |
| B04 | public API、JSON-RPC、`/ping`、`/api/mjpeg_live` | 待实现 |
| B05 | agent API、agent/browser WebSocket | 未合并（见 PARITY） |
| B06 | admin API | 待实现 |
| B13 | OAuth/OIDC | 已合并（crate）；HTTP 登录/绑定待服务器装配 |
| B14 | Web 静态资源、主题、隐藏节点过滤 | 已合并（crate）；待服务器装配 |

本文件只描述现有 Go 路由和规划状态，不新增未在 `cmd/server.go` 中出现的端点。
