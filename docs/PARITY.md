# 功能对等（Parity）汇总

本文档汇总 `komari-rs` 当前基线的模块状态、已知差异和迁移建议。
差异明细以 [parity-deviations.md](parity-deviations.md) 为准；本文件只做索引和
状态汇总，不重复修改 D 条目内容。

基线：第二轮合并后（B01–B21 已合并）。
模块状态**只在本文件维护**，其他文档引用这里。
详细配置见 [CONFIGURATION.md](CONFIGURATION.md)，迁移步骤见
[MIGRATION.md](MIGRATION.md)，部署见 [DEPLOYMENT.md](DEPLOYMENT.md)，路由见
[API.md](API.md)。

## 1. 模块状态

| 模块 | Crate | 状态 | 说明 |
| --- | --- | --- | --- |
| CLI / 启动配置 | `komari-cli`、`komari-config` | **已合并（B01/B15）** | 子命令、flag/env 优先级、日志、版本注入；CLI 布尔差异见 D-008。 |
| 数据库 | `komari-db` | **已合并（B02/B20/B21）** | SQLite schema 兼容、key/value `configs`、仓储层、历史迁移（D-003）。复合目标 `Vec<u8>` 残余见 D-022。 |
| 认证 | `komari-auth` | **已合并（B03）** | 密码、会话、TOTP、角色、私有站点中间件。 |
| 公开 API / JSON-RPC | `komari-api` | **已合并（B04/B06/B18）** | public API、JSON-RPC 与 `/api/mjpeg_live`（D-016–D-019）。 |
| Agent API / WebSocket | `komari-api` | **已合并（B05/B09）** | 注册、HTTP/WS 上报、`uploadBasicInfo`、浏览器实时数据。 |
| 管理 API | `komari-api` | **已合并（B06）** | `/api/admin/*` 全部分组；DDNS/通知脱敏已接入（D-009、D-011）。 |
| 通知 | `komari-notify` | **已合并（B07/B16）** | 8 个 provider（含 `Javascript`，`boa_engine` 执行用户脚本）；剩余差异见 D-001。 |
| GeoIP | `komari-geoip` | **已合并（B08/B21）** | mmdb/ip-api/ipinfo/geojs/empty，48h 缓存；初始化策略见 D-010。 |
| DDNS | `komari-ddns` | **已合并（B08）** | cloudflare/webhook/empty、重试、历史；删除扩展见 D-002。 |
| 记录维护 | `komari-jobs` | **已合并** | 保留期清理、15 分钟压缩、VACUUM/checkpoint。 |
| OAuth / OIDC | `komari-oauth` | **已合并（B13/B09）** | provider/回调/账户绑定、登录/绑定路由均已装配。 |
| Web / 静态资源 / 主题 | `komari-web` | **已合并（B14/B09/B15）** | 嵌入默认主题、`/favicon.ico`、`/themes/{id}/{path}`、SPA 回退；主题硬化见 D-020/D-021。 |
| 服务器装配 | `komari-api::server`、`komari-cli` | **已合并（B09/B17/B19）** | HTTP 监听、路由合并、后台任务、OAuth/GeoIP/DDNS、cloudflared 生命周期（D-012–D-015）。 |
| 文档 | `docs/` | **已合并（B11/B24）** | 配置、迁移、部署、API、parity 与 `check-docs.sh`。 |

### 当前可运行能力

`komari server` 会打开并迁移数据库、装配全部路由、启动后台任务并监听
`0.0.0.0:25774`。可直接使用：

- `komari server` 的 HTTP/WebSocket 服务、CORS、body limit、私有站点和优雅关闭；
- `/ping`、`/api/*`、`/api/rpc2`、浏览器/agent WebSocket、管理 API、OAuth 登录；
- 静态页面、默认/自定义主题、`/favicon.ico`、SPA 回退和临时分享 cookie；
- 通知、DDNS、GeoIP、记录保留/压缩、每日到期通知、cloudflared 隧道；
- `komari chpasswd`、`disable-2fa`、`permit-login`。

未实现：MySQL（Go 的 MySQL 分支同样被注释，生产应使用 SQLite）。

## 2. D 编号汇总

以下条目来自 [parity-deviations.md](parity-deviations.md)。**D 编号是稳定标识，
后续只追加，不重编号；B11 是唯一编号 owner，其他任务只提交内容。**

| 编号 | 主题 | 类型 | 影响 | 迁移建议 |
| --- | --- | --- | --- | --- |
| D-001 | Javascript 通知 provider | 已实现（B16），差异待确认 | 由 `boa_engine` 执行脚本；剩余差异见登记 | 无需改用其他 provider；按登记差异检查脚本。 |
| D-002 | DDNS 删除记录是 Rust 扩展 | 非 Go 扩展 | 仅 cloudflare 支持；empty 报错 | 不依赖该能力；回退 Go 时改用 provider API。 |
| D-003 | 旧库历史迁移 | 已合并（B20） | 极老库自动迁移 `client_infos` / OAuth / 通知配置 | 备份后直接切换；迁移失败会回滚。 |
| D-004 | GeoIP 缓存上限 10,000 | 已解决（B21） | 已移除上限，仅保留 48h TTL，与 Go 一致 | 无需处理。 |
| D-005 | `KOMARI_WS_DISABLE_ORIGIN` 字面量更宽松 | 已解决（B21） | 现在只接受大小写不敏感 `true` | 无需处理。 |
| D-006 | 邮件主题 Q vs B 编码 | 已解决（B21） | 已改为 Go 兼容 Q-encoding | 无需处理。 |
| D-007 | 配置数值越界严格报错 | 已解决（B21） | 已按 Go reflect 语义截断/回绕 | 无需处理。 |
| D-008 | CLI 布尔 flag 只接受 `true`/`false` | 有意差异，用户已确认保留 | `--flag=1`/`TRUE` 会以退出码 2 失败 | 改为 `--flag` 或 `--flag=true`。 |
| D-009 | DDNS provider 配置脱敏 | 有意安全差异（B08/B06） | 管理 API 不能读回 Cloudflare token/webhook 凭据 | 已接入 GET/list；写路径保留原值。 |
| D-010 | GeoIP mmdb 初始化只在缺失时下载 | 实现细节（B08） | 启动不会自动更新本地库 | 需要更新时调用管理端 `update_database`。 |
| D-011 | 通知 provider 配置脱敏 | 有意安全差异（B07/B06） | 管理 API 不能读回 bot token/密码等 | 已接入 GET/list；写路径保留原值。 |
| D-012 | cloudflared macOS 下载解包 `.tgz` | 有意修正（B17） | macOS 生成真正可执行文件；旧 `.tgz` 需删除 | macOS 切换前删除旧 `data/cloudflared`。 |
| D-013 | cloudflared 严格检查 HTTP 状态码 | 有意安全差异（B17） | 非 2xx 不安装响应体 | 确保可访问 GitHub Releases。 |
| D-014 | cloudflared 意外退出优雅关闭 | 有意差异（B17） | 退出码仍为 1，但先 drain 再退出 | 需要立即退出时缩短 `shutdown_timeout`。 |
| D-015 | `KOMARI_ENABLE_CLOUDFLARED` 布尔解析 | 已解决（B15） | 现在只接受大小写不敏感 `true` | 无需处理。 |
| D-016 | MJPEG 直接读仓储层而非 RPC | 有意实现差异（B18） | 不经过 RPC 审计包装；可见行为一致 | 无需操作。 |
| D-017 | MJPEG 文本度量偏移 1–2 px | 实现差异（B18） | 文字居中可能偏移 1–2 px | 像素级比对时以 Go 为准。 |
| D-018 | MJPEG JPEG 编码字节差异 | 实现差异（B18） | 尺寸/颜色/内容一致，字节流不同 | 逐字节比对时以 Go 为准。 |
| D-019 | MJPEG 字体下载测试开关 | 测试用扩展（B18） | 生产默认行为与 Go 一致 | 无需操作。 |
| D-020 | 主题上传接受 multipart | Rust 扩展（B15） | 同时兼容 multipart 与原始 ZIP | 回退 Go 时外部脚本改发原始 ZIP。 |
| D-021 | 主题下载/安装额外硬化 | 有意安全硬化（B15） | 大小/解压/超时/SSRF/原子安装限制 | 超大主题需调高常量重建。 |
| D-022 | 复合目标中的 `Vec<u8>` 不解 base64 | 已知残余（B21） | 仅影响嵌套 `[]byte` 的 base64 配置 | 避免该形状，或先按顶层 `Vec<u8>` 读取。 |

### B07 未占号差异

B07 的 offline 通知不触发 auto-renewal、周期循环由调用方负责这两项**未登记为
D 条目、不占号**；若后续需要正式登记，使用下一个可用编号（当前从 `D-023` 起）。

### 按影响分类

- **会阻断功能**：无。
- **已解决/已合并**：D-003、D-004、D-005、D-006、D-007、D-015。
- **行为差异但通常可接受**：D-002、D-008、D-010、D-012、D-014、D-016、D-017、
  D-018、D-019。
- **安全/API 契约相关**：D-009、D-011、D-013、D-020、D-021。
- **已实现、差异待确认**：D-001（B16 补齐 `Javascript` provider）。
- **已知残余**：D-022（嵌套 `Vec<u8>` 的 base64 解码）。

### 迁移时的最低检查

1. 通知可以使用 `Javascript`（D-001 已实现），剩余差异见 D-001 登记。
2. DDNS 不依赖删除记录（D-002）。
3. 极老数据库可直接切换；迁移失败会回滚（D-003）。
4. 旧脚本的 `--flag=1`/`--flag=TRUE` 改为 `--flag`/`--flag=true`（D-008）。
5. 需要更新 GeoIP 库时走管理端更新路径（D-010）。
6. macOS 切换前删除旧的 `data/cloudflared`（D-012）。
7. 主题上传若使用 multipart，回退 Go 时改为原始 ZIP（D-020）。
8. 含嵌套 `Vec<u8>` 的 base64 配置按 D-022 处理。

## 3. 未登记为 D 的功能缺口

当前没有“尚未实现”的 Go 功能缺口，因此不占用 D 编号。

- MySQL 未实现；Go 的 MySQL 分支也被注释，生产应使用 SQLite。
- `command_audits`、`ping_records`、`ping_tasks`、`task_results`、`tasks` 等表
  不由 Rust 创建/迁移；回退 Go 时需保留或由 Go 管理。
- 服务器端已装配 Public/Agent/Admin/RPC/OAuth/静态路由与浏览器 WS。

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
cargo test -p komari-api
cargo test -p komari-notify
cargo test -p komari-geoip
cargo test -p komari-ddns
cargo test -p komari-jobs
cargo test -p komari-oauth
cargo test -p komari-web
```

### 文档

```sh
./docs/check-docs.sh
```

## 5. 已知限制

- MySQL 未实现；Go 的 MySQL 分支也被注释，生产应使用 SQLite。
- `Javascript` 通知 provider 已由 `boa_engine` 实现（B16），复用常驻运行时；
  与 Go `goja` 的剩余差异登记在 D-001，待用户确认。
- 复合目标中的 `Vec<u8>` 仍未按 base64 解码（D-022）；当前持久化配置没有该形状。
- 参考库还有 `command_audits`、`ping_records`、`ping_tasks`、`task_results`、
  `tasks` 等表；Rust 不创建/迁移它们，回退时需保留或由 Go 管理。
- B07 已按协议删除独立 parity 文件，只提交代码/测试；D-011 由 B11 统一维护。
- 主题静态资源不实现 HTTP `Range`；预压缩文件只在主题包自带 `.gz`/`.br` 时提供。

## 6. 验收建议

1. 用生产库副本跑 `komari-db` compat 测试。
2. 用 `komari server` 在演练环境启动，确认 `/ping`、登录、agent 上报、管理 API、
   WebSocket、OAuth、静态页面和 MJPEG 均可用。
3. 逐项确认 D-001–D-022 的迁移建议。
4. 按 [API.md](API.md) 的路由清单做端到端回归。
5. 按 [DEPLOYMENT.md](DEPLOYMENT.md) 验证 systemd/Docker、反代 WS、备份和健康检查。

## 7. B09 跨 crate 变更说明

B09 的服务器装配需要少量跨 crate 改动，均为验收项所需的最小改动：

- `komari-auth`：新增 `BufferedBodyLimit` / `SkipBodyBuffering`，让普通请求的
  chunked 超限返回 413，并让 `/api/admin/upload/backup` 跳过 AuthLayer 的整包缓冲，
  保持 B06 的流式上传。
- `komari-config` / `komari-cli`：新增 `KOMARI_BODY_LIMIT` / `--body-limit`，用于
  配置普通请求体上限。
- `komari-notify`：新增 `renewal` 模块，供上线事件和每日 09:00 调度复用。
- `komari-oauth`：仅随 main 的 B04/B06 合并同步，B09 未改变 provider 语义。
- `docs`：更新服务器装配、OAuth、静态资源、浏览器 WS 与 body limit 状态。

B18 提交的 MJPEG 剩余差异已编号为 D-016–D-019；B15 的主题扩展/硬化已编号为
D-020/D-021；B21 的复合目标残余已编号为 D-022。B17 的 cloudflared 差异已编号为
D-012–D-015（其中 D-015 已由 B15 解决）。
