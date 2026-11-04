# 差分测试与前端 E2E

本文件说明如何把 Go 参考后端与 Rust 后端跑在同一组请求上做差分，以及如何
用真实 `komari-web` 构建产物做前端冒烟。差分只读业务代码，不修改根
`Cargo.toml` 的 workspace members。

- 差分入口：`scripts/differential-test.sh`
- 差分执行器：`scripts/differential_compare.py`
- 已知差异白名单：`scripts/differential-known-differences.json`
- 前端 E2E：`scripts/frontend-e2e.sh`

> 约定：`DF-xxx` 是**差分测试工具自己的编号**，只用于本文件和
> `differential-known-differences.json`。正式的 `D-xxx` 仍由 B11 在
> [parity-deviations.md](parity-deviations.md) 中维护；B23 只提交内容，不分配
> D 编号。

## 1. 快速开始

### 1.1 前置条件

| 工具 | 用途 |
| --- | --- |
| `cargo` / Rust 1.85+ | 构建 Rust 后端 |
| `go` 1.24+ | 构建 Go 参考后端 |
| `python3` | HTTP 驱动、SQLite 种子、JSON 归一化 |
| `curl` | 就绪探测和前端冒烟 |
| `sqlite3`（可选） | 手工查看临时数据库；脚本本身用 Python `sqlite3` |
| `npm` + 已安装的 `node_modules` | 前端 E2E 构建 |
| Chromium/Chrome（可选） | 浏览器 DOM 冒烟；没有则自动跳过 |

Go 参考仓库默认自动探测以下位置，也可以用 `--go-repo` / `KOMARI_GO_REPO`
显式指定：

```text
../komari
../komari/komari
../../komari/komari
../../github/komari/komari
/home/fearless/github/komari/komari
```

### 1.2 差分测试

```sh
# 自动构建 Go/Rust，自动选择空闲端口，临时目录 + 临时 SQLite
scripts/differential-test.sh

# 保留现场（临时目录、两个数据库、server.log、report.json）
scripts/differential-test.sh --keep

# 指定 Go 仓库/预构建二进制，跳过构建
scripts/differential-test.sh \
  --go-repo /path/to/go/komari \
  --go-bin /path/to/komari \
  --rust-bin target/debug/komari \
  --skip-build

# 固定端口、复制报告、严格模式（已知差异也失败）
scripts/differential-test.sh \
  --go-port 18080 --rust-port 18081 \
  --report-out /tmp/komari-diff-report.json \
  --strict
```

脚本行为：

1. 在 `mktemp` 临时目录下分别为两个后端创建 `data/`，使用各自的
   `data/komari.db`；**不会读取或修改用户现有的 `data/komari.db`**。
2. 通过 `ADMIN_USERNAME` / `ADMIN_PASSWORD` 让两边创建相同的测试管理员
   （默认 `admin` / `testpass`），不依赖交互式 CLI。
3. 等待 `/ping` 返回 `pong`；任一进程提前退出会打印对应 `server.log` 尾部。
4. 用同一份 SQL 向两个临时库插入：
   - 可见节点 `11111111-1111-4111-8111-111111111111`
   - 隐藏节点 `22222222-2222-4222-8222-222222222222`
   - 一条 `records` 和一条 `gpu_records`（同一时间戳）
5. 登录管理员，按第 2 节的矩阵重放请求。
6. 输出 `一致 / 已知差异 / 差异 / 仅一侧`，写 `report.json`，并清理进程与
   临时目录（`--keep` 除外）。

退出码：

| 退出码 | 含义 |
| --- | --- |
| `0` | 只有“一致”和已登记的“已知差异” |
| `1` | 存在“差异”或“仅一侧”（`--strict` 下已知差异也会失败） |
| `2` | 参数、依赖、构建或启动失败 |

### 1.3 前端 E2E

```sh
# 完整构建：npm run build -> 临时主题目录 -> KOMARI_DEFAULT_THEME_DIR 重编译
scripts/frontend-e2e.sh

# 保留现场和报告
scripts/frontend-e2e.sh --keep --report-out /tmp/komari-e2e-report.json

# 复用已有 dist（仍需要 komari-web 的 komari-theme.json/preview.png）
scripts/frontend-e2e.sh --skip-build --dist /path/to/komari-web/dist

# 只做 curl 检查，不启动浏览器
scripts/frontend-e2e.sh --no-browser
```

脚本会把 `npm run build` 的 `--outDir` 指到临时主题目录，避免覆盖
`komari-web` 或 Go 仓库里的构建产物；随后复制 `komari-theme.json` 和
`preview.png`（或历史拼写 `perview.png`），执行：

```sh
KOMARI_DEFAULT_THEME_DIR=<临时主题目录> cargo build -p komari-web
KOMARI_DEFAULT_THEME_DIR=<临时主题目录> cargo build -p komari-cli
```

启动 Rust server 后检查第 3 节的 HTTP 项。若环境里有 Chromium/Chrome，还会用
`--headless --dump-dom` 渲染 `/` 和 `/admin`；没有浏览器或浏览器无法启动时，
脚本会记录 `browser.skipped_reason` 并**只依赖 curl + HTML 关键字**，不会把
环境限制误报成功能失败。当前环境没有安装 Playwright/Puppeteer，因此没有做
点击/输入级交互；登录流程由 `POST /api/login` + 带 cookie 的 `/api/me` 覆盖。

## 2. 差分覆盖矩阵

所有请求都同时打到 Go（端口 A）和 Rust（端口 B），请求方法、路径、JSON body
和 cookie 完全一致。

| # | case | 请求 | 认证 | 覆盖点 |
| --- | --- | --- | --- | --- |
| 1 | `ping` | `GET /ping` | 无 | 健康检查、`text/plain`、`pong` |
| 2 | `version` | `GET /api/version` | 无 | 版本/哈希 envelope |
| 3 | `public` | `GET /api/public` | 无 | 公开站点设置、主题设置、默认值 |
| 4 | `nodes_guest` | `GET /api/nodes` | 无 | 访客节点列表、隐藏节点过滤、字段清理 |
| 5 | `me_guest` | `GET /api/me` | 无 | 未登录身份 |
| 6 | `recent_missing` | `GET /api/recent/<unknown>` | 无 | 无缓存报告时的空响应 |
| 7 | `records_missing` | `GET /api/records/load?uuid=<unknown>` | 无 | 空记录、`records:null`、`has_gpu_data:false` |
| 8 | `rpc_ping` | `POST /api/rpc2` `rpc.ping` | 无 | JSON-RPC 2.0 基本响应 |
| 9 | `rpc_public_info` | `POST /api/rpc2` `common:getPublicInfo` | 无 | RPC 公开信息 |
| 10 | `rpc_nodes` | `POST /api/rpc2` `common:getNodes` | 无 | RPC 节点 map、字段清理 |
| 11 | `rpc_latest` | `POST /api/rpc2` `common:getNodesLatestStatus` | 无 | 无在线 agent 时的空状态 |
| 12 | `rpc_version` | `POST /api/rpc2` `common:getVersion` | 无 | RPC 版本 |
| 13 | `login_bad_password` | `POST /api/login` | 无 | 错误凭据的 401 与错误文案 |
| 14 | `login` | `POST /api/login` | 无 | 登录成功、`Set-Cookie`、token 结构 |
| 15 | `me_admin` | `GET /api/me` | session | 管理员身份、2FA/SSO 字段 |
| 16 | `rpc_me_admin` | `POST /api/rpc2` `common:getMe` | session | RPC 管理员身份 |
| 17 | `nodes_admin` | `GET /api/nodes` | session | 管理员可见隐藏节点 |
| 18 | `admin_settings` | `GET /api/admin/settings`（跟随重定向） | session | 配置键/值、默认值落库差异 |
| 19 | `admin_settings_no_slash` | `GET /api/admin/settings`（不跟随） | session | 尾斜杠重定向差异 |
| 20 | `recent_seeded` | `GET /api/recent/<visible>` | 无 | 无内存报告时的空响应 |
| 21 | `records_seeded` | `GET /api/records/load?uuid=<visible>&hours=24` | 无 | 记录序列化、GPU 聚合、时间/数值格式 |
| 22 | `records_seeded_cpu` | 同上 + `load_type=cpu` | 无 | 过滤后的 `{client,time,cpu}` 形状 |
| 23 | `records_seeded_gpu` | 同上 + `load_type=gpu` | 无 | 两侧都拒绝 `gpu` 作为 load_type |
| 24 | `recent_hidden_guest` | `GET /api/recent/<hidden>` | 无 | 访客不能读取隐藏节点 |
| 25 | `records_hidden_guest` | `GET /api/records/load?uuid=<hidden>` | 无 | 访客不能读取隐藏节点记录 |

种子数据字段覆盖：`uuid/token/name/cpu_name/virtualization/arch/cpu_cores/os/
kernel_version/gpu_name/ipv4/ipv6/region/remark/public_remark/mem_total/
swap_total/disk_total/version/weight/price/billing_cycle/auto_renewal/currency/
expired_at/group/tags/ddns_*/hidden/traffic_limit*/created_at/updated_at`，以及
`records` 的全部指标和一条 `gpu_records`。

## 3. 前端 E2E 检查矩阵

| 检查 | 期望 |
| --- | --- |
| `GET /` | 200，`text/html`，含 `id="root"`、哈希化 `/assets/*.js` |
| `GET /assets/*.js` | 200，`javascript`，body > 1 KB |
| `GET /assets/*.css` | 200，`css`，body > 1 KB |
| `GET /admin` | 200，`text/html`，含 `id="root"` |
| `GET /ping` | 200，`text/plain`，body 为 `pong` |
| `GET /favicon.ico` | 200，`image/*` |
| `GET /manifest.json` | 200，JSON |
| `GET /api/public` | 200，`application/json`，`status=success` 且 `sitename` 非空 |
| `GET /api/version` | 200，`application/json`，`status=success` 且 `version` 非空 |
| `GET /unknown/spa/route` | 200，`text/html`，SPA 回退含 `id="root"` |
| `GET /api/does-not-exist` | 404（Rust 有意不让 API 404 变成 SPA） |
| `POST /api/login` | 200，`status=success`，返回 `session_token` |
| `GET /api/me`（带 cookie） | 200，`logged_in=true`，`username=admin` |
| `GET /api/admin/settings`（带 cookie） | 200，`status=success`，`data` 为对象 |
| 浏览器 `/`（可选） | 渲染后的 DOM 含 `Komari` |
| 浏览器 `/admin`（可选） | 渲染后的 DOM 含登录 UI 关键字 |

真实主题检查：若 `/` 返回的 HTML 含 `placeholder` 文本，E2E 直接失败，提示用
`KOMARI_DEFAULT_THEME_DIR` 重新构建；这样不会把占位主题当成“通过”。

## 4. 归一化与判定规则

### 4.1 归一化

- **JSON 对象键顺序**：递归排序后比较。
- **数组顺序**：按元素的 canonical JSON 排序后比较；本测试明确忽略列表顺序。
- **整数/浮点写法**：整数值的浮点（`80.0`、`0.0`、`50.0`）归一化为整数；
  非整数保留 6 位小数。Go 的 `float64(80)` 与 Rust 的 `80.0` 因此视为相等。
- **易变值**：登录返回的 `data.set-cookie.session_token` 被忽略并在报告中
  脱敏；`Set-Cookie` 头只比较“是否存在”，不比较值。
- **时间戳**：种子数据用同一个 `now` 写入两个库，所以 `records.time`、
  `clients.created_at/updated_at/expired_at` 可以直接比较；如果后续 case 返回
  服务器当前时间，可在对应 case 的 `ignore_paths` 中登记该路径。
- **管理员 UUID**：每个临时库随机生成，`me_admin` / `rpc_me_admin` 的
  `uuid` 登记为已知差异。
- **构建元数据**：`/api/version` 和 `common:getVersion` 的 `version` / `hash`
  登记为已知差异。
- **响应头**：只比较 `content-type` 的基础 MIME、`cache-control`、`location`
  和 `Set-Cookie` 是否存在；`date`、`content-length`、`etag`、`last-modified`、
  `vary` 不参与比较。
- **Content-Type charset**：Gin 输出 `application/json; charset=utf-8`，axum
  输出 `application/json`；基础 MIME 相同，charset 差异登记为 `DF-005`。

### 4.2 判定

| 分类 | 含义 | 是否失败 |
| --- | --- | --- |
| 一致 | 归一化后状态、头和 body 都相同 | 否 |
| 已知差异 | 命中 `differential-known-differences.json` 白名单 | 否（`--strict` 下失败） |
| 差异 | 同一路径上的值/状态/头不同，且未登记 | 是 |
| 仅一侧 | 某个 JSON key 只出现在 Go 或只出现在 Rust | 是 |

白名单结构：每个 case 可配置 `known_paths`（路径 → `{id, reason}`）、
`ignore_paths`、`known_go_only_keys`、`known_rust_only_keys`、`status_only` /
`known_status`。路径支持 `data.records[*].time` 这类数组通配。

## 5. 已知差异（DF）

这些是**当前差分测试观察到的、已登记且不阻塞**的差异。它们不是新的 D 编号；
B23 已把内容提交到 [parity-deviations.md](parity-deviations.md) 的“待 B11
编号”区，最终编号和措辞由 B11 决定。

### DF-001 新装默认站点描述不同

- **Go**：启动时 `config.GetManyAs[config.Legacy]()` 把 `config.Legacy` 的
  `default:"A simple server monitor tool."` 写入 `configs`；`/api/public` 和
  `common:getPublicInfo` 读到该值。
- **Rust**：`/api/public` 首次读取缺失键时使用
  `"Komari Monitor, a simple server monitoring tool."` 并写回。
- **影响**：全新数据库上 `data.description` / `result.description` 不同；
  管理端设置里也可见。已有数据库（已有 `description` 行）不受影响。
- **涉及 case**：`public`、`rpc_public_info`、`admin_settings`。

### DF-002 启动时配置默认值落库策略不同

- **Go**：`config.GetManyAs[config.Legacy]()` 在启动时把所有带 `default` tag
  的键写入 `configs`，因此 `GET /api/admin/settings` 一开始就有完整键集。
- **Rust**：只在各 API 首次读取时按需写回（例如 `/api/public` 读取的键）。
- **影响**：全新数据库上 Go 的 settings 多出
  `api_key`、`auto_discovery_key`、`base_scripts_url`、`ddns_enabled`、
  `ddns_sync_interval`、`eula_accepted`、`expire_notification_enabled`、
  `expire_notification_lead_days`、`login_notification`、
  `notification_enabled`、`notification_method`、`notification_template`、
  `script_domain`、`send_ip_addr_to_guest`、`traffic_limit_percentage`。
  两边对这些键的**读取默认值**一致；差异只在“行是否存在”。
- **涉及 case**：`admin_settings`。

### DF-003 `/api/admin/settings` 尾斜杠重定向

- **Go**（Gin）：`GET /api/admin/settings` 返回 `301`，`Location:
  /api/admin/settings/`；跟随重定向后才是 200 JSON。
- **Rust**（axum）：`/api/admin/settings` 和 `/api/admin/settings/` 都直接
  返回 200。
- **影响**：不跟随重定向的客户端会看到 301 vs 200；跟随重定向的客户端最终
  响应一致。
- **涉及 case**：`admin_settings_no_slash`。

### DF-004 版本/哈希构建元数据

- **Go**：`utils.CurrentVersion` 默认 `0.0.1`，`utils.VersionHash` 默认
  `unknown`（release 构建用 ldflags 注入）。
- **Rust**：`CARGO_PKG_VERSION`（当前 `0.1.0`），`KOMARI_VERSION_HASH` 默认
  `unknown`。
- **影响**：`/api/version` 和 `common:getVersion` 的 `version` 值不同；这不是
  API 契约差异，差分测试只登记不失败。
- **涉及 case**：`version`、`rpc_version`。

### DF-005 JSON Content-Type 的 charset

- **Go**（Gin）：`application/json; charset=utf-8`。
- **Rust**（axum）：`application/json`。
- **影响**：基础 MIME 相同；严格比较原始头会看到 charset 差异。
- **涉及 case**：所有 JSON 响应。

### DF-006 默认管理员 UUID 随机

- **两侧**：默认管理员的 `uuid` 都是随机生成的，临时数据库之间不可能相同。
- **影响**：`/api/me` 和 `common:getMe` 的 `uuid` 字段不同；登录 token 同理，
  但 token 已被忽略并脱敏。
- **涉及 case**：`me_admin`、`rpc_me_admin`。

## 6. 最近一次结果

运行日期：2026-11-04。环境：Linux x86_64，Rust `cargo 1.98.1`，Go
`go1.27.1`，Node `v26.12.1`，Chromium 可用。

### 6.1 差分

```sh
scripts/differential-test.sh --keep --report-out /tmp/b23-diff-report.json
```

| 指标 | 值 |
| --- | --- |
| case 数 | 25 |
| 一致 | 18 |
| 已知差异 | 23（DF-001/002/003/004/005/006） |
| 差异 | 0 |
| 仅一侧 | 0 |
| 退出码 | 0 |

报告：`/tmp/b23-diff-report.json`（用 `--report-out` 指定；重新运行会生成新的
临时目录，`--keep` 可保留现场）。

### 6.2 前端 E2E

```sh
scripts/frontend-e2e.sh --keep --report-out /tmp/b23-e2e-report.json
```

| 指标 | 值 |
| --- | --- |
| curl/DOM 检查 | 26 |
| 失败 | 0 |
| 浏览器 | 可用（Chromium headless） |
| `/` 渲染 | 通过 |
| `/admin` 登录 UI 渲染 | 通过 |

报告：`/tmp/b23-e2e-report.json`。

## 7. 限制与维护

- **没有 agent/WebSocket 上报路径**：本测试通过直接 SQLite 种子覆盖节点和
  记录序列化，但没有启动真实 agent，因此 `/api/recent/*` 的“有内存报告”
  分支、`common:getNodesLatestStatus` 的非空状态、WebSocket 上报未覆盖。
- **没有 MySQL**：两边都用 SQLite；MySQL 路径不在本测试范围。
- **没有 Playwright/Puppeteer**：当前 `komari-web` 的 `node_modules` 未安装
  它们；浏览器冒烟使用 Chromium `--dump-dom`，只验证渲染后的 DOM 关键字，
  不做点击/输入。需要交互级测试时再引入 Playwright。
- **不触碰用户数据**：两个后端都在临时目录中运行，数据库、主题、`data/`
  全部是临时产物；用户现有 `data/komari.db` 不会被读写。
- **更新白名单**：新增/修改已知差异时，同时更新
  `scripts/differential-known-differences.json` 和本文件的 DF 小节；若属于
  正式对等差异，按 [parity-deviations.md](parity-deviations.md) 的规则提交
  给 B11 编号，不要自行分配 D 编号。
