# 差分测试与前端 E2E

本文件说明如何把 Go 参考后端与 Rust 后端跑在同一组请求上做差分，以及如何
用真实 `komari-web` 构建产物做前端冒烟。

- 差分入口：`scripts/differential-test.sh`
- 差分执行器：`scripts/differential_compare.py`
- 已知差异白名单：`scripts/differential-known-differences.json`
- 前端 E2E：`scripts/frontend-e2e.sh`

> **差分只验证两侧一致，不证明行为正确。** 因此脚本除了逐字段比较，还对
> 隐藏节点过滤、节点数量、records 数量/顺序、GPU 设备、默认描述、完整 settings
> 键集等做显式不变量断言；这些断言失败也会让脚本以非零退出。
>
> 约定：`DF-xxx` 是**差分测试工具自己的编号**，只用于本文件和
> `differential-known-differences.json`。正式的 `D-xxx` 仍由 B11 在
> [parity-deviations.md](parity-deviations.md) 中维护。DF 编号不复用；
> DF-001/DF-002/DF-003 已修复并退役，当前只剩 DF-004/DF-005/DF-006。

## 1. 快速开始

### 1.1 前置条件

| 工具 | 用途 |
| --- | --- |
| `cargo` / Rust 1.85+ | 构建 Rust 后端 |
| `go` 1.24+ | 构建 Go 参考后端（`--skip-build` 且提供 `--go-bin` 时不需要） |
| `python3` | HTTP 驱动、SQLite 种子、JSON 归一化、不变量断言 |
| `curl` | 就绪探测和前端冒烟 |
| `sqlite3`（可选） | 手工查看临时数据库；脚本本身用 Python `sqlite3` |
| `npm` + `node_modules` | 仅前端 E2E 的完整构建需要；`--skip-frontend-build` 不需要 |
| Chromium/Chrome（可选） | 浏览器 DOM 冒烟；没有则记录 skipped |

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

# 使用预构建二进制（--skip-build 必须显式给 --go-bin，避免误用陈旧二进制）
scripts/differential-test.sh \
  --skip-build \
  --go-bin /path/to/go/komari \
  --rust-bin target/debug/komari

# 固定端口、复制报告、严格模式（已知差异也失败）
scripts/differential-test.sh \
  --go-port 18080 --rust-port 18081 \
  --report-out /tmp/komari-diff-report.json \
  --strict

# 开发期间允许脏工作区（默认脏工作区会以退出码 2 拒绝运行）
scripts/differential-test.sh --allow-dirty
```

脚本行为：

1. 默认要求 Go 与 Rust 源码工作区干净（`git status --porcelain` 为空），
   并记录两侧 `git rev-parse HEAD`、dirty 状态、二进制 SHA-256；开发期可用
   `--allow-dirty` 放宽为警告。
2. 在 `mktemp` 临时目录下分别为两个后端创建 `data/`，使用各自的
   `data/komari.db`；**不会读取或修改用户现有的 `data/komari.db`**。
3. 通过 `ADMIN_USERNAME` / `ADMIN_PASSWORD` 让两边创建相同的测试管理员
   （默认 `admin` / `testpass`），不依赖交互式 CLI。
4. 等待 `/ping` 返回 `pong`；任一进程提前退出会打印对应 `server.log` 尾部。
5. 用同一份 SQL 向两个临时库插入：
   - 可见节点 `11111111-1111-4111-8111-111111111111`
   - 隐藏节点 `22222222-2222-4222-8222-222222222222`
   - 3 条不同时间、不同指标的 `records`
   - GPU 设备 0 的 3 条记录和设备 1 的 1 条记录
6. 登录管理员，按第 2 节的矩阵重放请求。
7. 输出 `一致 / 已知差异 / 差异 / 仅一侧`，写 `report.json`，并清理进程与
   临时目录（`--keep` 除外）。`--workdir` 复用时会先清空 `go/data` 与
   `rust/data` 并打印警告，避免读到上一轮的库。

退出码：

| 退出码 | 含义 |
| --- | --- |
| `0` | 只有“一致”和已登记的“已知差异” |
| `1` | 存在“差异”“仅一侧”或不变量失败（`--strict` 下已知差异也会失败） |
| `2` | 参数、依赖、构建、脏工作区、端口或启动失败 |

### 1.3 前端 E2E

```sh
# 完整构建：在临时副本中 npm run build -> 临时主题目录 -> 重编译 Rust
scripts/frontend-e2e.sh

# 保留现场和报告
scripts/frontend-e2e.sh --keep --report-out /tmp/komari-e2e-report.json

# 复用已有 dist；仍然会用该 dist 重编译 Rust，保证 asset hash 一致
scripts/frontend-e2e.sh --skip-frontend-build --dist /path/to/komari-web/dist

# 只做 curl 检查，不启动浏览器
scripts/frontend-e2e.sh --no-browser
```

`--skip-build` 仍作为 `--skip-frontend-build` 的兼容别名保留，但语义已经
改变：它只跳过前端构建，**不会**跳过 `KOMARI_DEFAULT_THEME_DIR` 重编译。

前端构建在 `$WORK/web-build` 的临时副本中进行，`node_modules` 通过符号链接
复用；因此 Vite 的 `bundle-analysis.html`、TypeScript build info 等产物不会
改写用户的 `komari-web` 工作区。随后复制 `komari-theme.json` 和
`preview.png`（或历史拼写 `perview.png`），执行：

```sh
KOMARI_DEFAULT_THEME_DIR=<临时主题目录> cargo build -p komari-web
KOMARI_DEFAULT_THEME_DIR=<临时主题目录> cargo build -p komari-cli
```

启动 Rust server 后检查第 3 节的 HTTP 项，并校验服务端 `index.html` 引用的
asset hash 与 `$THEME/dist/index.html` 一致。若环境里有 Chromium/Chrome，还会用
`--headless --dump-dom` 渲染 `/` 和 `/admin`；要求 DOM 中出现
`data-is-root-theme="true"`、`#root` 有子元素且 DOM 大于静态 index，避免被
静态 `<title>Komari` 误通过。浏览器缺失或已知环境错误（缺共享库、sandbox）
记为 `skipped`（`ok=false`，不增加 failures）；未知非零退出记为 failure。
当前环境没有安装 Playwright/Puppeteer，因此没有做点击/输入级交互；登录流程
由 `POST /api/login` + 带 cookie 的 `/api/me` 覆盖。

## 2. 差分覆盖矩阵

所有请求都同时打到 Go（端口 A）和 Rust（端口 B），请求方法、路径、JSON body
和 cookie 完全一致。

| # | case | 请求 | 认证 | 覆盖点 |
| --- | --- | --- | --- | --- |
| 1 | `ping` | `GET /ping` | 无 | 健康检查、`text/plain`、`pong` |
| 2 | `version` | `GET /api/version` | 无 | 版本/哈希 envelope |
| 3 | `public` | `GET /api/public` | 无 | 公开站点设置、主题设置、默认描述 |
| 4 | `nodes_guest` | `GET /api/nodes` | 无 | 访客节点列表、隐藏节点过滤、字段清理 |
| 5 | `me_guest` | `GET /api/me` | 无 | 未登录身份 |
| 6 | `recent_missing` | `GET /api/recent/<unknown>` | 无 | 无缓存报告时的空响应 |
| 7 | `records_missing` | `GET /api/records/load?uuid=<unknown>` | 无 | 空记录、`records:null`、`has_gpu_data:false` |
| 8 | `rpc_ping` | `POST /api/rpc2` `rpc.ping` | 无 | JSON-RPC 2.0 基本响应 |
| 9 | `rpc_public_info` | `POST /api/rpc2` `common:getPublicInfo` | 无 | RPC 公开信息 |
| 10 | `rpc_nodes` | `POST /api/rpc2` `common:getNodes` | 无 | RPC 节点 map、字段清理 |
| 11 | `rpc_latest` | `POST /api/rpc2` `common:getNodesLatestStatus` | 无 | 无在线 agent 时的空状态 |
| 12 | `rpc_version` | `POST /api/rpc2` `common:getVersion` | 无 | RPC 版本 |
| 13 | `login_bad_password` | `POST /api/login` | 无 | 错误凭据的 401 与错误文案（用户名来自 `--admin-user`） |
| 14 | `login` | `POST /api/login` | 无 | 登录成功、`Set-Cookie`、token 形状 |
| 15 | `me_admin` | `GET /api/me` | session | 管理员身份、2FA/SSO 字段、UUID 形状 |
| 16 | `rpc_me_admin` | `POST /api/rpc2` `common:getMe` | session | RPC 管理员身份、UUID 形状 |
| 17 | `nodes_admin` | `GET /api/nodes` | session | 管理员可见隐藏节点 |
| 18 | `admin_settings` | `GET /api/admin/settings`（跟随重定向） | session | 配置键/值、完整默认键集 |
| 19 | `admin_settings_no_slash` | `GET /api/admin/settings`（不跟随） | session | 301 + `Location` |
| 20 | `admin_settings_post_no_slash` | `POST /api/admin/settings`（不跟随） | session | 307 + `Location`，不执行写入 |
| 21 | `recent_seeded` | `GET /api/recent/<visible>` | 无 | 无内存报告时的空响应 |
| 22 | `records_seeded` | `GET /api/records/load?uuid=<visible>&hours=24` | 无 | 记录序列化、顺序、GPU 聚合、时间/数值格式 |
| 23 | `records_seeded_cpu` | 同上 + `load_type=cpu` | 无 | 过滤后的 `{client,time,cpu}` 形状 |
| 24 | `records_seeded_gpu` | 同上 + `load_type=gpu` | 无 | 两侧都拒绝 `gpu` 作为 load_type |
| 25 | `recent_hidden_guest` | `GET /api/recent/<hidden>` | 无 | 访客不能读取隐藏节点 |
| 26 | `records_hidden_guest` | `GET /api/records/load?uuid=<hidden>` | 无 | 访客不能读取隐藏节点记录 |

种子数据字段覆盖：`uuid/token/name/cpu_name/virtualization/arch/cpu_cores/os/
kernel_version/gpu_name/ipv4/ipv6/region/remark/public_remark/mem_total/
swap_total/disk_total/version/weight/price/billing_cycle/auto_renewal/currency/
expired_at/group/tags/ddns_*/hidden/traffic_limit*/created_at/updated_at`，以及
`records` 的全部指标、3 条不同时间记录、2 个 GPU 设备。

### 2.1 显式不变量

除两侧逐字段比较外，脚本还断言（失败即退出码 1）：

- 访客 `/api/nodes` 只返回 1 个可见节点，管理员返回 2 个节点。
- `/api/public` 的默认描述是 `A simple server monitor tool.`。
- `records_seeded` 的 `count` 与数组长度均为 3，时间升序。
- `gpu_devices` 同时包含设备 `0`（3 条）和 `1`（1 条），`has_gpu_data=true`。
- `load_type=cpu` 返回 3 条过滤记录。
- `/api/admin/settings` 包含 Go `config.Legacy` 的完整 30 个默认键。
- 隐藏节点的 recent/records 请求在两侧都返回 400 `UUID is required`。
- 管理员 `/api/me` 返回 `logged_in=true` 且 `username` 与测试账号一致。

## 3. 前端 E2E 检查矩阵

| 检查 | 期望 |
| --- | --- |
| `GET /` | 200，`text/html`，含 `id="root"`、哈希化 `/assets/*.js` |
| 服务端 asset hash | 与 `$THEME/dist/index.html` 的 JS 引用一致 |
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
| `GET /api/admin/settings/`（带 cookie） | 200，`status=success`，`data` 为对象 |
| 浏览器 `/`（可选） | 渲染后的 DOM 含 `data-is-root-theme`、非空 `#root`、dashboard 文案，且大于静态 index |
| 浏览器 `/admin`（可选） | 渲染后的 DOM 含 `data-is-root-theme`、非空 `#root`、登录 UI 关键字 |

真实主题检查：若 `/` 返回的 HTML 含 `placeholder` 文本，E2E 直接失败，提示用
`KOMARI_DEFAULT_THEME_DIR` 重新构建；这样不会把占位主题当成“通过”。

## 4. 归一化与判定规则

### 4.1 归一化

- **JSON 对象键顺序**：递归排序后比较。
- **数组顺序**：**保留顺序**；`nodes` / `records` / GPU 记录的顺序属于契约。
  只有对象键无序。
- **整数/浮点写法**：整数值的浮点（`80.0`、`0.0`、`50.0`）归一化为整数；
  非整数保留 6 位小数。Go 的 `float64(80)` 与 Rust 的 `80.0` 因此视为相等。
- **易变值**：登录返回的 `data.set-cookie.session_token` 先校验为非空且长度
  ≥ 16，再忽略并在报告中脱敏；`Set-Cookie` 头只比较“是否存在”，不比较值。
- **管理员 UUID**：先校验两侧都是合法 UUID，再登记为已知差异；空值/非法值
  不会被跳过，会直接判为差异。
- **版本/哈希**：先校验为非空字符串，再登记为已知差异。
- **时间戳**：种子数据用同一组 `times` 写入两个库，所以 `records.time`、
  `clients.created_at/updated_at/expired_at` 可以直接比较；如果后续 case 返回
  服务器当前时间，可在对应 case 的 `ignore_paths` 中登记该路径。
- **响应头**：只比较 `content-type` 的基础 MIME、`cache-control`、`location`
  和 `Set-Cookie` 是否存在；`date`、`content-length`、`etag`、`last-modified`、
  `vary` 不参与比较。
- **Content-Type charset**：Gin 输出 `application/json; charset=utf-8`，axum
  输出 `application/json`；基础 MIME 相同，charset 差异登记为 `DF-005`。
- **重定向 case**：`status_only` 同时比较状态码和 `Location`；GET 301、POST 307。

### 4.2 判定

| 分类 | 含义 | 是否失败 |
| --- | --- | --- |
| 一致 | 归一化后状态、头和 body 都相同，且不变量通过 | 否 |
| 已知差异 | 命中 `differential-known-differences.json` 白名单且形状校验通过 | 否（`--strict` 下失败） |
| 差异 | 同一路径上的值/状态/头不同，且未登记 | 是 |
| 仅一侧 | 某个 JSON key 只出现在 Go 或只出现在 Rust | 是 |
| 不变量失败 | 两侧可能一致，但不符合显式语义断言 | 是 |

白名单结构：每个 case 可配置 `known_paths`（路径 → `{id, reason}`）、
`validators`（易变值的形状校验）、`ignore_paths`、`status_only`。路径支持
`data.records[*].time` 这类数组通配。

## 5. 已知差异（DF）

### 5.1 已修复并退役

| 编号 | 原差异 | 最终状态 |
| --- | --- | --- |
| `DF-001` | 新装默认站点描述不同 | **已修复**：两侧统一使用 `komari_config::DEFAULT_DESCRIPTION`（`A simple server monitor tool.`）。白名单与 parity 条目已删除。 |
| `DF-002` | 启动默认键落库策略不同 | **已修复**：Rust 启动时在 `prepare_database` 中幂等 seed 全部 `SiteConfig` 默认键。白名单与 parity 条目已删除。 |
| `DF-003` | `/api/admin/settings` 尾斜杠重定向不同 | **已修复**：Rust 在鉴权前对 GET/HEAD 返回 301、其他方法返回 307，并保留 query；与 Gin 一致。白名单与 parity 条目已删除。 |

DF-001/002/003 编号保留但**不再复用**；它们不再出现在
`differential-known-differences.json` 的 `cases` 中，只在 `retired` 里留档。

### 5.2 当前保留（工具级已知差异）

#### DF-004 版本/哈希构建元数据

- **Go**：`utils.CurrentVersion` 默认 `0.0.1`，`utils.VersionHash` 默认
  `unknown`（release 构建用 ldflags 注入）。
- **Rust**：`CARGO_PKG_VERSION`（当前 `0.1.0`），`KOMARI_VERSION_HASH` 默认
  `unknown`。
- **影响**：`/api/version` 和 `common:getVersion` 的 `version` 值不同；这不是
  API 契约差异，差分测试先校验非空字符串，再登记为 known。
- **涉及 case**：`version`、`rpc_version`。

#### DF-005 JSON Content-Type 的 charset

- **Go**（Gin）：`application/json; charset=utf-8`。
- **Rust**（axum）：`application/json`。
- **影响**：基础 MIME 相同；严格比较原始头会看到 charset 差异。差分测试只比较
  基础 MIME，并把 charset 差异聚合为一条 known finding。
- **涉及 case**：所有 JSON 响应。

#### DF-006 默认管理员 UUID 随机

- **两侧**：默认管理员的 `uuid` 都是随机生成的，临时数据库之间不可能相同。
- **影响**：`/api/me` 和 `common:getMe` 的 `uuid` 字段不同；差分测试先校验
  两侧都是合法 UUID，再登记为 known。登录 token 同理，但 token 只校验形状并
  脱敏，不进入报告。
- **涉及 case**：`me_admin`、`rpc_me_admin`。

> DF-004/005/006 是差分工具层面的构建元数据/头/UUID 差异，不提交为正式
> `D-xxx`；如未来需要正式登记，由 B11 按 [parity-deviations.md](parity-deviations.md)
> 的规则编号。

## 6. 最近一次结果

运行日期：2026-11-04。环境：Linux x86_64，Rust `cargo 1.98.1`，Go
`go1.27.1`，Node `v26.12.1`，Chromium 可用。

### 6.1 差分

```sh
scripts/differential-test.sh --report-out /tmp/b23-diff-report.json
```

| 指标 | 值 |
| --- | --- |
| case 数 | 26 |
| match_cases | 22 |
| known_cases | 4 |
| diff_cases | 0 |
| known_findings | 5（DF-004×2、DF-005×1、DF-006×2） |
| diff_findings | 0 |
| only_findings | 0 |
| invariant_failures | 0 |
| 退出码 | 0 |

报告：`/tmp/b23-diff-report.json`（用 `--report-out` 指定；重新运行会生成新的
临时目录，`--keep` 可保留现场）。

### 6.2 前端 E2E

```sh
scripts/frontend-e2e.sh --report-out /tmp/b23-e2e-report.json
```

| 指标 | 值 |
| --- | --- |
| 检查项 | 27 |
| 失败 | 0 |
| skipped | 0 |
| 浏览器 | 可用（Chromium headless） |
| `/` 渲染 | 通过（`data-is-root-theme` + 非空 `#root` + dashboard 文案） |
| `/admin` 登录 UI 渲染 | 通过 |

`--no-browser` 运行：25 项检查、0 失败、0 skipped，浏览器标记为不可用。
`CHROME_BIN` 指向“No usable sandbox”假脚本时：2 项 skipped、0 失败、退出码 0；
指向未知崩溃假脚本时：2 项失败、退出码 1。

## 7. 限制与维护

- **差分只证明一致，不证明正确**：语义正确性由 2.1 的显式不变量覆盖；新增
  API 时应同时补充不变量，而不是只依赖两侧一致。
- **没有 agent/WebSocket 上报路径**：本测试通过直接 SQLite 种子覆盖节点和
  记录序列化，但没有启动真实 agent，因此 `/api/recent/*` 的“有内存报告”
  分支、`common:getNodesLatestStatus` 的非空状态、WebSocket 上报未覆盖。
- **没有 MySQL**：两边都用 SQLite；MySQL 路径不在本测试范围。
- **没有 Playwright/Puppeteer**：当前 `komari-web` 的 `node_modules` 未安装
  它们；浏览器冒烟使用 Chromium `--dump-dom`，只验证渲染后的 DOM，不做
  点击/输入。需要交互级测试时再引入 Playwright。
- **不触碰用户数据/前端工作区**：两个后端都在临时目录中运行；前端在临时副本
  中构建，`bundle-analysis.html` 等产物不会写回 `komari-web`。
- **更新白名单**：新增/修改已知差异时，同时更新
  `scripts/differential-known-differences.json` 和本文件的 DF 小节；DF 编号
  只用于本工具，正式对等差异按 [parity-deviations.md](parity-deviations.md)
  的规则提交给 B11 编号。
