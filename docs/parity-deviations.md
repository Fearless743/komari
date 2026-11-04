# 与 Go 实现的已知差异登记

本文件登记 `komari-rs` 相对 Go 版**有意保留或已知存在**的行为差异，供迁移和
功能对等验收逐项确认。每条包含原因、影响与迁移方式。

维护规则：

- **B11 是本文件唯一的编号 owner**：新增条目统一由 B11 分配下一个编号，当前为
  `D-023`；其他任务只提交内容，不要自行重编号。
- **B07 合并协议**：B07 已按协议删除独立 `docs/parity-deviations.md`，只提交
  代码/测试；通知脱敏内容由本文件的 `D-011` 统一维护。后续 B07 变更不再新增
  独立 parity 文件，也不新增编号。
- B07 的其它差异（offline 通知不触发 auto-renewal、周期循环由调用方负责）当前
  **不登记、不占号**；若后续登记，使用下一个可用编号（当前从 `D-023` 起）。
- 既有条目的编号和内容不要改写；确需修正时保留编号并注明变更。
- 已解决的历史差异保留编号，状态改为“已解决”，并注明解决它的任务；不要删除
  条目或复用编号。
- 仅登记可复现、能从代码或 crate README 找到依据的差异。
- 模块“尚未实现”属于功能缺口，登记在 [PARITY.md](PARITY.md)，不占用 D 编号。

基线：第二轮合并后（B01–B21 已合并）；本文件由 B11/B24 统一维护。

## D-001 通知 Javascript provider（已实现，保留编号）

- **状态**：已实现（B16），存在已登记差异，**待用户确认**。编号保留。
- **Rust**：`komari-notify` 的 `Javascript` provider 使用 `boa_engine`（纯 Rust
  ECMAScript 引擎）执行用户脚本：
  - 每个 provider 实例一个常驻运行时线程，`Init` 只执行一次脚本，`sendMessage`
    / `sendEvent` 复用同一 realm，与 Go 的 `JavaScriptSender.vm` 一致；
  - `sendMessage(message, title)` 必需（boolean 或 Promise），`sendEvent(event)`
    可选且缺失/不可调用时回退到文本格式；
  - 注入 `fetch`、`XMLHttpRequest`、`console.log/error`、`setTimeout`、
    `require`（任何模块都抛 `Invalid module`，与 goja 未注册原生模块一致）；
  - 单次调用 30 秒墙钟超时（含同步代码），`Init` 加载脚本不计时，与 Go 的
    `SendTextMessage` 计时起点一致。
- **Go**：通过 `goja` + `goja_nodejs/require` 执行同一份脚本。
- **剩余差异**：
  - **错误文本**：只返回错误类型与入口函数名（如
    `JavaScript error in sendMessage: TypeError`），不返回 Go 的完整错误消息，
    避免脚本字面量中的 token 进入 API/日志（安全差异）；
  - **同步死循环**：Go 只在轮询 Promise 时检查超时，同步 `while(true)` 会永久
    挂起。Rust 对纯 JS 死循环通常先触发 Boa 的每栈帧 1,000,000 次循环迭代上限，
    返回 `Maximum loop iteration limit ... exceeded` 脚本错误；这类错误由
    `execute()` 直接返回，不 poison/重建运行时，后续调用继续复用同一 realm。
    只有不迭代的同步阻塞代码超过 30 秒墙钟超时才走外层超时，并在超时后重建
    运行时；Go 对两种情况都会一直挂起；
  - **statusText**：`reqwest`/`http` 丢弃自定义 reason phrase，Rust 回退到标准
    原因短语（`202 Accepted`）；传输层可观测时（测试/自定义 transport）使用
    wire reason；
  - **XHR readyState**：2/3 在响应到达时与 4 同批触发（Go 按网络阶段触发）；
    同步 XHR 通过 Tokio 运行时阻塞等待；
  - **setTimeout**：只在入口 Promise 仍 pending 时触发（Go 的 goroutine 在返回
    后仍会执行）；非可调用参数与 Go 一样是 no-op；
  - **event JSON**：
    - `clients` 为空 `Vec` 时序列化为 `null`。Go 的 nil slice 是 `null`，但
      **非 nil 空 slice 是 `[]`**，`komari-db` 的 `Vec` 无法区分两种情况；
    - `clients[]` 的字段集合与 Go 的 json tags 一致：两侧对 `uuid`、`token`、
      `ipv4`、`ipv6`、`remark`、`public_remark`、`version` 都使用 omitempty，
      空值同样被省略；唯一文本差异是 `price` 的 `0` 在 Rust 序列化为 `0.0`
      （JS 中 `0 === 0.0`，不可观察）；
    - `time` 由 `LocalTime` 重新按**应用时区**渲染（RFC3339Nano）。Go 的
      `time.Time` 保留原 location，若事件时间来自非应用时区，偏移会不同；
  - **headers.get()**：`fetch` 的 `response.headers` 除普通属性外还提供
    大小写不敏感的 `get(name)`；Go 的对象只有属性，没有 `get` 方法（扩展）；
  - **console**：输出到 `tracing` 而非 stdout（Go 用 `fmt.Println`）；
  - **引擎**：boa 与 goja 的 ES 实现存在细微差异（如部分 Annex B / 旧版扩展）。
- **影响**：`notification_method=Javascript` 可以正常发送；不再需要改用其他
  provider。
- **迁移**：使用自定义脚本的部署可直接沿用 Go 版脚本；若脚本依赖
  `require('console')`、`console.info`、`clearTimeout` 或 `process`，需要改为
  Go 也支持的写法（这些在 Go 中同样不可用）。

## D-002 DDNS 记录删除是 Rust 扩展

- **Rust**：`DdnsProvider::delete_record` / `DdnsService::delete_record` 是可选
  异步方法；`cloudflare` 实现 `DELETE /zones/{zone}/dns_records/{id}`，
  `empty` 返回错误，其余 provider 使用默认的“不支持”实现。
- **Go**：`factory.IDdnsProvider` 只有 `Sync`，没有删除记录的接口。
- **原因**：允许调用方清理 Cloudflare 记录，同时不破坏 Go 兼容的 provider 接口。
- **影响**：这是 **非 Go 对等扩展**；不要假设所有 provider 都能删除，`empty` 和
  未实现的 provider 会明确报错。
- **迁移**：从 Go 迁到 Rust 不需要使用该能力；从 Rust 回退 Go 时，依赖删除逻辑的
  外部脚本需要改为直接调用 provider API。
- **状态**：非 Go 扩展，已记录在 `crates/komari-ddns/README.md`。

## D-003 旧数据库历史迁移（已合并）

- **Rust**：`komari-db` 的 `migrate()` 在单个事务中执行以下 Go 历史迁移：
  - `client_infos` 合并到 `clients`，原表重命名为 `client_infos_backup`；
  - pre-1.0.2 将 OAuth/通知设置从旧 `configs` 列搬出到
    `oidc_providers` / `message_sender_providers`；
  - 现有加法式 schema 迁移、缺失列补齐和 legacy `configs` 转换保持不变。
- **Go**：启动时 `MergeDatabase` + `AutoMigrate` 会执行上述历史迁移。
- **差异**：Rust 把旧 `configs` 表保留为 `configs_legacy_backup`，Go 会直接
  删除/重写；旧凭据列不会复制到新的 key/value `configs`，只保留在备份表中。
- **迁移**：无需再先用 Go 启动一次；直接备份数据库后切换即可。
- **状态**：已合并（B20），由 `crates/komari-db/tests/legacy_migration.rs`
  覆盖合并、重命名、OIDC/通知迁移、幂等、缺列、空表、异常 JSON、回滚和
  现有库不变。

## D-004 GeoIP 缓存容量上限 10,000

- **Rust（B21 前）**：`GeoIpService` 的查询缓存容量上限为 `10_000` 条，超出后按最早写入
  淘汰；TTL 仍为 48 小时。
- **Go**：`go-cache` 只设置 48 小时过期，没有显式容量上限。
- **原因**：长时间运行时限制内存占用。
- **影响**：唯一 IP 超过 10,000 个时，Rust 会更早淘汰缓存，可能增加对
  ipinfo/ip-api/geojs 的上游请求；mmdb 本地查询不受网络影响。
- **迁移**：无需处理；当前实现与 Go 一样只按 48 小时 TTL 过期。
- **状态**：已解决（B21），与 Go 一致；编号保留。
- **变更（B21）**：已删除 `DEFAULT_CACHE_CAPACITY`、`with_cache_capacity` 和最旧
  条目淘汰逻辑，缓存只按 48 小时 TTL 惰性过期。上文的 10,000 条上限已不存在。

## D-005 `KOMARI_WS_DISABLE_ORIGIN` 布尔字面量更宽松

- **Rust（B21 前）**：启动配置的布尔解析接受 `true` / `1` / `yes` / `on`（大小写不敏感），
  都视为关闭 Origin 校验。
- **Go**：`ws.CheckOrigin` 只把大小写不敏感的 `true` 当作关闭校验；`1`、`yes`、
  `on` 不会生效。
- **原因**：Rust 统一使用配置层的布尔解析，和 `KOMARI_ALLOW_CORS` 等变量保持一致。
- **影响**：部署脚本写 `KOMARI_WS_DISABLE_ORIGIN=1` 时，Rust 会关闭校验而 Go 不会；
  从 Rust 回退 Go 时可能出现“配置看似没变但行为变了”。
- **迁移**：跨版本部署统一使用 `true` / `false`；不要依赖 `1`/`yes`/`on`。
- **状态**：已解决（B21），与 Go 一致；编号保留。
- **变更（B21）**：`KOMARI_WS_DISABLE_ORIGIN` 改用 `parse_strict_true`，只接受
  大小写不敏感的 `true`（不 trim）；`1`/`yes`/`on`/带空白值一律为 false，且不
  产生配置错误。上文的宽松行为已移除。

## D-006 通知邮件主题的 RFC 2047 编码不同

- **Rust（B21 前）**：使用 `lettre` 的 RFC 2047 **B-encoding** 编码非 ASCII 主题。
- **Go**：使用 `mime.QEncoding` 的 **Q-encoding**。
- **原因**：两套库的默认实现不同，但都符合 RFC 2047。
- **影响**：邮件主题的原始字节不同；邮件客户端解码后内容一致。若有严格的签名/
  归档比对，可能看到差异。
- **迁移**：无需处理；当前实现已与 Go 逐字节一致。
- **状态**：已解决（B21），与 Go 一致；编号保留。
- **变更（B21）**：邮件主题改用 Go `mime.QEncoding.Encode` 兼容的 Q-encoding
  （空格转 `_`、非 ASCII 字节转大写十六进制、每个 encoded-word 最多 63 个内容
  字符），并通过预编码 raw header 写入，不再使用 lettre 默认的 B-encoding。

## D-007 配置数值越界时严格报错

- **Rust（B21 前）**：`ConfigRepo::get_as<T>` 对超出目标整数范围的值返回错误（例如把
  `70000` 读成 `u16`，或把负数读成无符号类型）；不再继续截断/回绕。
- **Go**：`config.GetAs` 通过 `reflect` 转换，窄整数可能截断，负数转无符号可能
  回绕。
- **原因**：尽早暴露损坏或手改错的配置值，避免静默产生错误阈值。
- **影响**：旧库中若存在越界数值，Rust 读取该键会失败；相关功能可能回退默认值或
  报错，而不是使用回绕后的值。
- **迁移**：无需处理；当前实现按 Go 的 reflect 语义截断/回绕。
- **状态**：已解决（B21），与 Go 一致；编号保留。
- **变更（B21）**：`get_as` 复现 Go `config.GetAs` 的 `convertAndSet`：float→int
  向零截断，窄整数按 2^bits 回绕（`300.0` 读成 `i8` 为 `44`），负数转无符号
  回绕，越界 float 使用 amd64 的 `i64::MIN` / `2^63`，不再返回错误。复合目标
  另见 D-022。

## D-008 CLI 布尔 flag 只接受小写 true/false

- **Rust**：clap 的 `BoolValueParser` 只接受 `true` / `false`；`--flag` 等价于
  `--flag=true`。`--flag=1`、`--flag=TRUE`、`--flag=yes` 会报 `invalid value`
  并以退出码 `2` 退出。
- **Go**：pflag 的布尔 flag 使用 `strconv.ParseBool`，接受 `1`、`t`、`T`、
  `TRUE`、`true`、`True`、`0`、`f`、`F`、`FALSE`、`false`、`False`。
- **原因**：clap 默认布尔解析器更严格，能尽早暴露脚本中的拼写/类型错误。
- **影响**：旧部署脚本中的 `--allow-cors=1`、`--enable-cloudflared=TRUE` 等会在
  Rust 版直接失败，而不是启用选项。
- **迁移**：统一改为 `--flag`（启用）、`--flag=true` 或 `--flag=false`；
  环境变量仍可使用 `1/0`、`yes/no`、`on/off`。
- **状态**：有意差异，用户已确认保留（B24）。

## D-009 DDNS provider 配置脱敏

- **Rust**：`ddns_providers.addition` 在 SQLite 中保持完整，内部同步使用完整值；
  但管理 API 返回前必须脱敏。使用
  `komari_ddns::redact_addition(provider_name, &addition)` 处理存储行，或
  `komari_ddns::redact_provider_configuration(&provider)` 处理实时 provider。
- **Go**：管理 API 直接返回 `models.DdnsProvider`，包含原始 `addition` JSON。
- **原因**：避免 API 客户端读回 Cloudflare token、webhook URL/headers/body 等
  凭据。
- **脱敏字段**：

  | Provider | API 响应移除 | 保留 |
  | --- | --- | --- |
  | `cloudflare` | `api_token`、`zone_id` | `record_id`、`record_type`、`ttl`、`proxied` |
  | `webhook` | `url`、`headers`、`body` | `method`、`content_type`、`response_json_path` |
  | `empty` | 无 | 全部 |

- **规则**：敏感字段是删除而不是打码；非法 JSON、`null`、数组等非对象值返回
  `{}`；未知 provider 返回 `{}`；webhook URL 一律移除（路径/查询串可能含 token）；
  原始 `addition` 和数据库行不被修改；Cloudflare/Webhook 的 `Debug` 输出脱敏；
  解析失败使用固定错误文案，HTTP 错误在写日志/同步历史前移除请求 URL。
- **影响**：API 客户端不能读回凭据；修改敏感字段时必须重新提交完整值。
- **迁移**：管理 API 的 GET/list 响应路径已调用脱敏函数；写路径继续使用原始
  `addition`。
- **状态**：有意安全差异，已合并（B08/B06）。

## D-010 GeoIP 本地数据库初始化

- **Rust**：`mmdb` 初始化优先加载已有数据库，只有文件缺失时才下载；管理端
  `GeoIpService::update_database` 始终下载并重载。
- **Go**：`NewMaxMindGeoIPService` 在文件缺失时下载，`UpdateDatabase` 始终下载。
- **原因**：避免每次启动都覆盖本地 mmdb，同时保留管理端强制更新能力。
- **影响**：启动时不会自动获取最新数据库；需要更新时调用管理端更新路径。
- **迁移**：首次部署可预置 `data/GeoLite2-Country.mmdb`，或让首次初始化下载；
  定期更新使用管理端接口。
- **状态**：实现细节，已合并 B08。

## D-011 通知 provider 配置脱敏

- **Rust**：`komari_notify::redact_addition(provider, &addition)` 已合并（B07），
  返回脱敏后的 `serde_json::Value`；数据库中的
  `message_sender_providers.addition` 保持完整，内部 sender 使用完整配置，且脱敏
  结果不得写回数据库。
- **Go**：管理 API 直接返回 `message_sender_providers.addition`，包含 token。
- **原因**：避免 API 客户端读回通知凭据。
- **脱敏字段**：

  | Provider | 移除字段 |
  | --- | --- |
  | `telegram` | `bot_token` |
  | `bark` | `device_key` |
  | `Server酱³` | `api_url`（含 sendkey）、`sendkey`、`send_key`、`token` |
  | `Server酱Turbo` | `api_url`（含 sendkey）、`sendkey`、`send_key`、`token` |
  | `webhook` | `headers`、`body`、`password`；`url`/`URL` 在含 userinfo、凭据查询参数或 Slack/Discord/Teams 路径 token 时移除 |
  | `email` | `password` |
  | `Javascript` | `script`、`code` |
  | `empty` | 无 |

- **URL 处理**：所有 URL 形状字段（`url`、`URL`、`server_url`、`endpoint`、
  `api_url`）都检查；凭据检测覆盖 userinfo、常见签名查询参数、Slack/Discord/
  Teams 路径 token 和 Telegram bot token；`author`、`monkey` 等无关键不误删。
- **日志**：`Debug` 输出只保留 scheme + host + port，隐藏路径和查询串；敏感 header
  （`Authorization`、`X-Auth`、`X-Hub-Signature-256`、`X-Api-Key` 等）脱敏；
  请求/响应 body 不打印；远程响应 body/描述不进入错误。
- **已知启发式限制**：任意路径段中的 token 无法被 API 脱敏器自动识别；`Debug`
  输出仍安全，因为总是隐藏路径。
- **调用点**：管理 API 的 GET/list 响应路径调用 `redact_addition`；写路径始终
  使用原始 `addition`。
- **状态**：已合并（B07/B06）。

## D-012 cloudflared macOS 下载会解包 `.tgz`

- **Rust**：macOS 目标下载 `cloudflared-darwin-*.tgz` 后，从归档中提取
  `cloudflared` 成员写入 `data/cloudflared`，再设置 `0755`。
- **Go**：`utils/cloudflared` 把 `.tgz` 响应直接写入 `data/cloudflared` 并尝试
  执行，没有解包步骤；在 macOS 上通常无法直接运行。
- **原因**：让 macOS 上的隧道实际可用。
- **影响**：Rust 生成的 `data/cloudflared` 是真正的可执行文件；从 Go 迁移时，如果
  `data/cloudflared` 已存在且实际是 `.tgz`，Rust 会按“已存在则复用”直接使用并
  启动失败。
- **迁移**：macOS 从 Go 切到 Rust 前删除旧的 `data/cloudflared`，让 Rust 重新下载
  并解包；其他平台不受影响。
- **状态**：有意修正，已合并（B17）。

## D-013 cloudflared 下载严格检查 HTTP 状态码

- **Rust**：`HttpDownloader` 只在 `2xx` 时写入文件；非 `2xx` 返回
  `CloudflaredError::Download`，不会安装响应体。
- **Go**：`http.Get` 后不检查 `StatusCode`，会把 404/重定向错误页当作可执行文件
  写入 `data/cloudflared`。
- **原因**：避免把 HTML 错误页安装成二进制并产生难以诊断的启动错误。
- **影响**：GitHub 不可达、代理返回 403/404 时 Rust 启动失败并保留原文件；Go 可能
  先写入错误内容再启动失败。两者都不会启动隧道，但磁盘状态不同。
- **迁移**：确保服务器可访问 GitHub Releases；失败时按错误提示检查代理/网络。
- **状态**：有意安全差异，已合并（B17）。

## D-014 cloudflared 意外退出的关闭方式与退出码

- **Rust**：记录子进程退出码，向 HTTP 服务发送关闭信号，按
  `shutdown_timeout` 完成 drain 后让 `serve` 返回错误，CLI 最终以退出码 `1`
  结束。
- **Go**：在等待 goroutine 中记录日志后直接 `os.Exit(1)`，HTTP 服务没有机会
  graceful shutdown。
- **原因**：Rust 库代码不应直接终止进程；同时保留 Go 的非零退出码语义。
- **影响**：退出码一致（`1`）；Rust 会短暂等待连接排空并执行后台任务清理，Go 立即
  退出。若 `shutdown_timeout` 内仍有长连接，Rust 会在超时后强制结束。
- **迁移**：依赖退出码的编排逻辑无需修改；需要立即退出时可缩短
  `ServerConfig::shutdown_timeout`。
- **状态**：有意差异，已合并（B17）。

## D-015 `KOMARI_ENABLE_CLOUDFLARED` 布尔解析（已解决）

- **Rust（B17 时）**：沿用配置层布尔解析，接受 `true` / `1` / `yes` / `on`
  （大小写不敏感）。
- **Go**：`cmd/server.go` 只把 `strings.ToLower(value) == "true"` 视为启用。
- **原因**：当时与 `KOMARI_ALLOW_CORS` 共用解析器。
- **影响**：部署脚本写 `KOMARI_ENABLE_CLOUDFLARED=1` 时 Rust 会启动隧道而 Go 不会。
- **迁移**：无需处理；当前实现已与 Go 一致。
- **状态**：已解决（B21），与 Go 一致；编号保留。该差异由 B17 登记，B21 修复。
- **变更（B21）**：`KOMARI_ENABLE_CLOUDFLARED` 改用 `parse_strict_true`，只接受
  大小写不敏感的 `true`（不 trim）；`1`/`yes`/`on`/带空白值均为 false，且不报
  配置错误。CLI 覆盖值仍按 D-008 的 clap 布尔规则解析。

## D-016 MJPEG 直接读取仓储层而不是 guest JSON-RPC

- **Rust**：`/api/mjpeg_live` 直接读取 `clients`、hub 最新 report 缓存和配置；
  不调用 `common:getNodes` / `common:getNodesLatestStatus`。
- **Go**：通过 `jsonRpc.OnInternalRequest(ctx, "guest", ...)` 获取节点与状态。
- **原因**：Rust 的 MJPEG 渲染器与 RPC 层解耦，避免每个 2 秒帧走一次内部 RPC。
- **影响**：guest 的隐藏节点过滤、`send_ip_addr_to_guest` 掩码、状态与在线判断
  已按 Go 行为复刻；但不会经过 RPC 的审计/权限包装。MJPEG 始终按 guest 处理，
  因此没有可见的权限差异。
- **迁移**：无需操作；若后续要求 MJPEG 走统一 RPC 审计，再改为内部 RPC 调用。
- **状态**：有意实现差异，已合并（B18）。

## D-017 MJPEG 文本度量偏移 1–2 px

- **Rust**：TTF 文本宽度使用 `ab_glyph` 的 advance/kerning。
- **Go**：文本宽度使用 `font.BoundString` 的 bearing 边界。
- **原因**：Rust 侧字体库的度量实现不同；布局参数（1280 px、字号、行距）一致。
- **影响**：文字居中可能偏移 1–2 px；帧尺寸、颜色和内容一致。
- **迁移**：无需操作；需要像素级比对时以 Go 输出为准。
- **状态**：实现差异，已合并（B18）。

## D-018 MJPEG JPEG 编码字节级差异

- **Rust**：JPEG 使用 `jpeg-encoder`（4:2:0、quality 90）。
- **Go**：JPEG 使用 `image/jpeg`（4:2:0、quality 90）。
- **原因**：编码器实现不同，采样率和质量参数一致。
- **影响**：JPEG 字节流与 Go 不完全相同，但尺寸、颜色和内容一致。
- **迁移**：无需操作；需要逐字节比对时以 Go 输出为准。
- **状态**：实现差异，已合并（B18）。

## D-019 MJPEG 字体下载测试开关

- **Rust**：`ApiConfig::mjpeg_font_download` 默认 `true`，测试可设为 `false`
  以避免访问 `https://hyperos.mi.com/font-download/MiSans.zip`。
- **Go**：没有对应配置，始终在 `data/font.ttf` 缺失时尝试下载。
- **原因**：保证离线/CI 测试确定性。
- **影响**：生产默认行为与 Go 一致；该字段不是用户配置项。
- **迁移**：无需操作。
- **状态**：仅测试用的 Rust 扩展，已合并（B18）。

## D-020 主题上传同时接受 multipart 和原始 ZIP

- **Rust**：只有 `PUT /api/admin/theme/upload` 同时接受 `multipart/form-data` 的
  `file` 字段和 Go 的原始 ZIP 请求体；`update`/`import` 仍是 JSON 元数据请求，
  不接受 multipart 或原始 ZIP，下载到的 ZIP 与上传包走同一安装逻辑。
- **Go**：只读取原始 ZIP 请求体，不解析 multipart。
- **原因**：内置管理界面以 multipart 上传，保持 Go 原始请求体的同时兼容该扩展。
- **影响**：这是 **非 Go 对等扩展**；Go 客户端行为不变，Rust 客户端可多一种上传
  方式。响应与错误体仍与 Go 对齐。
- **迁移**：从 Rust 回退 Go 时，使用 multipart 的外部脚本需要改为发送原始 ZIP
  请求体。
- **状态**：Rust 扩展，已合并（B15）。

## D-021 主题下载与安装的额外硬化

- **Rust**：在 Go 的主题上传/更新/导入语义之上增加：
  - 上传/下载各 64 MiB 上限；
  - 解压后 128 MiB 与条目数上限（防 zip bomb）；
  - 单次请求 30 秒超时；
  - 跟随重定向时重新检查目标，防止 SSRF 绕过；
  - 先解压到暂存目录再原子安装，失败不会删除已安装的同 `short` 主题。
- **Go**：没有这些上限、重定向复查或原子安装步骤。
- **原因**：主题包来自管理端和远程 URL，需要限制资源消耗并避免失败安装破坏现有
  主题。
- **影响**：超大或恶意主题包在 Rust 会以 400/错误响应拒绝，Go 可能继续处理；
  正常大小的主题行为一致。
- **迁移**：无需操作；若依赖超大主题包，需要调高 `crates/komari-api` 中的常量
  并重新构建。
- **状态**：有意安全硬化，已合并（B15）。

## D-022 复合目标中的 `Vec<u8>` 仍不按 base64 解码

- **Rust**：`ConfigRepo::get_as<T>` 对顶层 `Vec<u8>` 目标按 Go
  `base64.StdEncoding` 语义解码（忽略 CR/LF、允许非零 trailing bits，失败时回退
  原始字符串字节）；但 `struct`/`slice`/`map` 内部的 `Vec<u8>` 字段不会解码
  base64 字符串，会报 `invalid type: string ..., expected a sequence`。
- **Go**：`encoding/json` 对任意层级的 `[]byte` 字段都按 base64 解码；只有顶层
  标量转换才走 `convertAndSet` 的 reflect 回退。
- **原因**：B21 的 `GoValue` 只在目标本身是序列/字节时拦截字符串，serde 在结构体
  字段反序列化时直接使用 `serde_json` 的严格语义。
- **影响**：只有“复合目标里嵌套 `[]byte` 且存储为 base64 字符串”的配置会读取
  失败；当前仓库的持久化配置没有这种形状，因此生产配置不受影响。顶层
  `Vec<u8>` 和复合目标里的数值 remarshal 已与 Go 对齐。
- **迁移**：避免把 base64 字符串写入复合配置的 `[]byte` 字段；需要时先按顶层
  `Vec<u8>` 读取，或在 Go 侧改写为数字数组。
- **状态**：已知残余差异（B21），待后续修复或用户确认。
