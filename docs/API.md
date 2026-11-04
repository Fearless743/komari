# Rust API 路由概览

本文档列出 Komari Go 版的现有路由，以及它们在 `komari-rs` 当前基线中的实现状态。
第二轮合并后，下列路由均已由 Rust 代码提供；行为差异以
[parity-deviations.md](parity-deviations.md) 为准。

基线：第二轮合并后（B01–B21 已合并）。
**模块状态只在 [PARITY.md](PARITY.md) 维护**，本文只描述路由范围和 Go 参考。

## 1. 当前状态

- `crates/komari-api` 已实现 public/agent/admin API、JSON-RPC、浏览器与 agent
  WebSocket，并由 `komari-api::server` 装配成可监听的 HTTP 服务。
- `crates/komari-oauth`（B13）的 provider/回调/账户绑定与 HTTP 登录/绑定路由均已
  装配（B09）。
- `crates/komari-web`（B14）的静态资源/主题/SPA 路由已合并进服务器 fallback。
- `komari server` 会监听 `0.0.0.0:25774`，本文列出的路由均可通过 HTTP 使用。

模块状态以 [PARITY.md](PARITY.md) 为准。

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
| 健康检查 | `GET /ping`（Go 用 `Any`） | 已实现（B04 public router；B09 默认装配） | B04/B09 |
| 公开 API | `/api/login`、`/api/public`、`/api/nodes`、`/api/version` 等 | 已实现（B04） | B04 |
| Agent API | `/api/clients/report`、`/api/clients/uploadBasicInfo` | 已实现（B05/B09） | B05/B09 |
| 浏览器 WebSocket | `GET /api/clients` | 已实现（B05/B09） | B05/B09 |
| 管理 API | `/api/admin/*` | 已实现（B06） | B06 |
| JSON-RPC | `GET/POST /api/rpc2` | 已实现（B04/B06） | B04/B06 |
| OAuth | `/api/oauth`、`/api/oauth_callback` | 已实现（B04，B09 装配） | B04/B09 |
| 静态资源 | `/`、`/themes/:id/*path`、`/favicon.ico` | 已实现（komari-web + B09 装配） | B09 |
| MJPEG | `/api/mjpeg_live` | 已实现（Go 布局、MiSans 字体下载/回退、`lang`/`tz_offset`） | B18 |

## 4. 公开路由（Go 参考，Rust 状态）

> B09 默认装配 B04 public、B06 admin/rpc、浏览器 WS 与静态资源。`/ping` 与
> `/api/oauth*` 由 B04 public router 提供，B09 内置副本不会重复挂载。具体状态以
> [PARITY.md](PARITY.md) 为准。

以下端点来自 `komari/cmd/server.go`：

| 方法 | 路径 | 说明 | Rust 责任 |
| --- | --- | --- | --- |
| `ANY` | `/ping` | 返回纯文本 `pong`。 | B04 |
| `POST` | `/api/login` | 密码登录，设置 `session_token`。 | B04 |
| `GET` | `/api/me` | 当前身份信息。 | B04 |
| `GET` | `/api/clients` | 浏览器实时数据 WebSocket（不是普通 JSON）。 | B05/B09 |
| `GET` | `/api/nodes` | 节点列表/公开信息。 | B04 |
| `GET` | `/api/public` | 公开站点设置。 | B04 |
| `GET` | `/api/oauth` | 发起 OAuth 登录。 | B13 |
| `GET` | `/api/oauth_callback` | OAuth 回调。 | B13 |
| `GET` | `/api/logout` | 注销并清除会话。 | B04 |
| `GET` | `/api/version` | 服务端版本。 | B04 |
| `GET` | `/api/recent/:uuid` | 指定节点最近记录。 | B04 |
| `GET` | `/api/records/load` | 按 UUID/时间范围读取记录。 | B04 |
| `GET` | `/api/mjpeg_live` | MJPEG 实时流。 | B04/B18 |

`/api/clients` 是 WebSocket：客户端发送 `get` 或 `get <uuid>`，服务端返回
`{"status":"success","data":{"online":[...],"data":{...}}}`。未登录时过滤
`hidden` 节点、private-site 校验与 `session_token` 独立校验由 B09 实现。

## 5. Agent 路由

### 5.1 实现状态

以下端点已由 `komari-api::client_router` 实现，并由 B09 装配进服务器；边界与剩余
差异以 [PARITY.md](PARITY.md) 为准。

### 5.2 路由

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/api/clients/report` | agent 持久 WebSocket 上报。 |
| `POST` | `/api/clients/report` | 一次性 HTTP 上报。 |
| `POST` | `/api/clients/uploadBasicInfo` | 静态硬件/OS/网络信息上报。 |
| `GET` | `/api/clients` | 浏览器 WebSocket 实时数据。 |

## 6. 管理路由（Rust 已实现，B06）

以下路由均来自 Go `cmd/server.go`，需要 `Admin` 角色，已由 `komari-api::admin`
实现并装配。

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
| `GET` / `POST` | `/api/rpc2` | 已实现（B04/B06） |

Go 的 JSON-RPC 方法分发在 `api/jsonRpc/`。Rust 的 `komari-api::rpc` 已实现
`/api/rpc2` 的 POST 与 WebSocket 分发，包含 `guest`/`client`/`admin`/`common`
命名空间和 Go 的错误码。Go 没有 `admin:*` RPC 方法；Rust 的扩展方法默认关闭，
设置 `KOMARI_RPC_ADMIN_METHODS=true` 才会注册。

## 8. 静态资源与 SPA

Go 版由 `public.Static` 提供：

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/favicon.ico` | 优先 `data/favicon.ico`，其次当前主题 `dist/favicon.ico`。 |
| `GET` | `/themes/:id/*path` | 自定义主题文件。 |
| `GET` | 其他未匹配路径 | SPA 回退到 `dist/index.html`，并注入站点名/描述/custom head/body。 |

Rust 的 `komari-web`（B14）已实现这些路由：`routes` / `routes_for` 提供
`/favicon.ico` 与 `/themes/{id}/{path}`，`serve_fallback` 提供 SPA/静态回退。
服务器装配（B09）已把 `routes_for` 合并进主 Router，并把 `serve_fallback` 作为
fallback；`komari server` 会直接提供页面和主题资源。

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

成功响应由各 handler 决定（Go 版常见 `{"status":"success", ...}`），Rust 的
handler 已按 Go 的形状返回。

## 10. 责任边界

| 任务 | 范围 | 当前状态 |
| --- | --- | --- |
| B04 | public API、JSON-RPC、`/ping`、`/api/mjpeg_live` | 已合并（B04；MJPEG 由 B18 补齐） |
| B05 | agent API、agent/browser WebSocket | 已合并（B05/B09） |
| B06 | admin API | 已合并（B06） |
| B13 | OAuth/OIDC | 已合并（B13/B09） |
| B14 | Web 静态资源、主题、隐藏节点过滤 | 已合并（B14/B09） |

本文件只描述现有 Go 路由和规划状态，不新增未在 `cmd/server.go` 中出现的端点。
