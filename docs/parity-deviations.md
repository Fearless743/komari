# 与 Go 实现的已知差异登记

本文件登记 `komari-rs` 相对 Go 版**有意保留或已知存在**的行为差异，供迁移和
功能对等验收逐项确认。每条包含原因、影响与迁移方式。

维护规则：

- **B11 是本文件唯一的编号 owner**：新增条目统一由 B11 分配下一个编号，当前为
  `D-012`；其他任务只提交内容，不要自行重编号。
- **B07 合并协议**：B07 已按协议删除独立 `docs/parity-deviations.md`，只提交
  代码/测试；通知脱敏内容由本文件的 `D-011` 统一维护。后续 B07 变更不再新增
  独立 parity 文件，也不新增编号。
- B07 的其它差异（offline 通知不触发 auto-renewal、周期循环由调用方负责）当前
  **不登记、不占号**；若后续登记，使用 `D-012` / `D-013`。
- 既有条目的编号和内容不要改写；确需修正时保留编号并注明变更。
- 仅登记可复现、能从代码或 crate README 找到依据的差异。
- 模块“尚未实现”属于功能缺口，登记在 [PARITY.md](PARITY.md)，不占用 D 编号。

基线：`main@0382eb3`（B07/B08/B10/B13/B14）；本文件由 B11 统一维护。

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
    挂起；Rust 由外层 30 秒墙钟超时返回错误，并重建运行时（Go 会一直挂起）。
    另有每栈帧 1,000,000 次循环迭代上限；
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

- **Rust**：`GeoIpService` 的查询缓存容量上限为 `10_000` 条，超出后按最早写入
  淘汰；TTL 仍为 48 小时。
- **Go**：`go-cache` 只设置 48 小时过期，没有显式容量上限。
- **原因**：长时间运行时限制内存占用。
- **影响**：唯一 IP 超过 10,000 个时，Rust 会更早淘汰缓存，可能增加对
  ipinfo/ip-api/geojs 的上游请求；mmdb 本地查询不受网络影响。
- **迁移**：通常无需处理；需要更大缓存时调整
  `crates/komari-geoip/src/service.rs` 的 `DEFAULT_CACHE_CAPACITY` 并重新构建。
- **状态**：有意差异，待 B08 对等验收确认。

## D-005 `KOMARI_WS_DISABLE_ORIGIN` 布尔字面量更宽松

- **Rust**：启动配置的布尔解析接受 `true` / `1` / `yes` / `on`（大小写不敏感），
  都视为关闭 Origin 校验。
- **Go**：`ws.CheckOrigin` 只把大小写不敏感的 `true` 当作关闭校验；`1`、`yes`、
  `on` 不会生效。
- **原因**：Rust 统一使用配置层的布尔解析，和 `KOMARI_ALLOW_CORS` 等变量保持一致。
- **影响**：部署脚本写 `KOMARI_WS_DISABLE_ORIGIN=1` 时，Rust 会关闭校验而 Go 不会；
  从 Rust 回退 Go 时可能出现“配置看似没变但行为变了”。
- **迁移**：跨版本部署统一使用 `true` / `false`；不要依赖 `1`/`yes`/`on`。
- **状态**：有意差异（更宽松），待 B05/B14 WS 验收确认。

## D-006 通知邮件主题的 RFC 2047 编码不同

- **Rust**：使用 `lettre` 的 RFC 2047 **B-encoding** 编码非 ASCII 主题。
- **Go**：使用 `mime.QEncoding` 的 **Q-encoding**。
- **原因**：两套库的默认实现不同，但都符合 RFC 2047。
- **影响**：邮件主题的原始字节不同；邮件客户端解码后内容一致。若有严格的签名/
  归档比对，可能看到差异。
- **迁移**：通常无需处理；需要逐字节一致时在投递前统一编码。
- **状态**：实现差异（语义等价），已记录在 `crates/komari-notify/README.md`。

## D-007 配置数值越界时严格报错

- **Rust**：`ConfigRepo::get_as<T>` 对超出目标整数范围的值返回错误（例如把
  `70000` 读成 `u16`，或把负数读成无符号类型）；不再继续截断/回绕。
- **Go**：`config.GetAs` 通过 `reflect` 转换，窄整数可能截断，负数转无符号可能
  回绕。
- **原因**：尽早暴露损坏或手改错的配置值，避免静默产生错误阈值。
- **影响**：旧库中若存在越界数值，Rust 读取该键会失败；相关功能可能回退默认值或
  报错，而不是使用回绕后的值。
- **迁移**：检查 `configs` 表中的数值（尤其是 `ddns_sync_interval`、
  `record_preserve_time`、`expire_notification_lead_days`、
  `traffic_limit_percentage`），改为合法范围。
- **状态**：兼容性限制，已记录在 `crates/komari-db/README.md`。

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
- **状态**：有意差异，待 B01 对等验收确认。

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
- **迁移**：B06 的 GET/list 响应路径必须调用脱敏函数；写路径继续使用原始
  `addition`。
- **状态**：有意安全差异，已合并 B08。

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
- **调用点**：B06 的 GET/list 响应路径调用 `redact_addition`；写路径始终使用原始
  `addition`。
- **状态**：已合并（B07）；待 B06 接入 API 并对等验收。

## 待 B11 编号（D-012 起）

以下条目由 B17 提交内容，编号与最终措辞由 B11 统一分配。

### cloudflared macOS 下载会解包 `.tgz`

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
- **状态**：有意修正，待 B17 对等验收确认。

### cloudflared 下载严格检查 HTTP 状态码

- **Rust**：`HttpDownloader` 只在 `2xx` 时写入文件；非 `2xx` 返回
  `CloudflaredError::Download`，不会安装响应体。
- **Go**：`http.Get` 后不检查 `StatusCode`，会把 404/重定向错误页当作可执行文件
  写入 `data/cloudflared`。
- **原因**：避免把 HTML 错误页安装成二进制并产生难以诊断的启动错误。
- **影响**：GitHub 不可达、代理返回 403/404 时 Rust 启动失败并保留原文件；Go 可能
  先写入错误内容再启动失败。两者都不会启动隧道，但磁盘状态不同。
- **迁移**：确保服务器可访问 GitHub Releases；失败时按错误提示检查代理/网络。
- **状态**：有意安全差异，待 B17 对等验收确认。

### cloudflared 意外退出的关闭方式与退出码

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
- **状态**：有意差异，待 B17 对等验收确认。

### `KOMARI_ENABLE_CLOUDFLARED` 布尔字面量更宽松

- **Rust**：沿用配置层布尔解析，接受 `true` / `1` / `yes` / `on`（大小写不敏感）。
- **Go**：`cmd/server.go` 只把 `strings.ToLower(value) == "true"` 视为启用。
- **原因**：与 `KOMARI_ALLOW_CORS`、`KOMARI_WS_DISABLE_ORIGIN` 共用解析器，行为
  一致（参见 D-005）。
- **影响**：部署脚本写 `KOMARI_ENABLE_CLOUDFLARED=1` 时 Rust 会启动隧道而 Go 不会；
  从 Rust 回退 Go 时可能出现“配置没变但隧道不再启动”。
- **迁移**：跨版本部署统一使用 `true` / `false`。
- **状态**：有意差异（更宽松），待 B17 对等验收确认。

## 待 B11 编号（B18 提交）

以下条目由 B18 提交，按维护规则不自行分配 D 编号，等待 B11 合并时编号。

### MJPEG 直接读取仓储层而不是 guest JSON-RPC

- **Rust**：`/api/mjpeg_live` 直接读取 `clients`、hub 最新 report 缓存和配置；
  不调用 `common:getNodes` / `common:getNodesLatestStatus`。
- **Go**：通过 `jsonRpc.OnInternalRequest(ctx, "guest", ...)` 获取节点与状态。
- **原因**：Rust 的 MJPEG 渲染器与 RPC 层解耦，避免每个 2 秒帧走一次内部 RPC。
- **影响**：guest 的隐藏节点过滤、`send_ip_addr_to_guest` 掩码、状态与在线判断
  已按 Go 行为复刻；但不会经过 RPC 的审计/权限包装。MJPEG 始终按 guest 处理，
  因此没有可见的权限差异。
- **迁移**：无需操作；若后续要求 MJPEG 走统一 RPC 审计，再改为内部 RPC 调用。

### MJPEG 文本度量与 JPEG 编码器的字节级差异

- **Rust**：TTF 文本宽度使用 `ab_glyph` 的 advance/kerning；JPEG 使用
  `jpeg-encoder`（4:2:0、quality 90）。
- **Go**：文本宽度使用 `font.BoundString` 的 bearing 边界；JPEG 使用
  `image/jpeg`（4:2:0、quality 90）。
- **原因**：Rust 侧库的度量/编码实现不同，但采样率、质量、布局参数一致。
- **影响**：文字居中可能偏移 1–2 px；JPEG 字节流与 Go 不完全相同，但尺寸、
  颜色和内容一致。
- **迁移**：无需操作；需要逐字节比对时以 Go 输出为准。

### MJPEG 字体下载测试开关

- **Rust**：`ApiConfig::mjpeg_font_download` 默认 `true`，测试可设为 `false`
  以避免访问 `https://hyperos.mi.com/font-download/MiSans.zip`。
- **Go**：没有对应配置，始终在 `data/font.ttf` 缺失时尝试下载。
- **原因**：保证离线/CI 测试确定性。
- **影响**：生产默认行为与 Go 一致；该字段不是用户配置项。
- **迁移**：无需操作。

## 待 B11 编号（B23 提交）

以下条目由 B23 的差分测试（`scripts/differential-test.sh`）发现并提交内容，
按维护规则不自行分配 D 编号，等待 B11 合并时编号。差分测试的临时编号
（`DF-001` 等）只用于 [DIFFERENTIAL.md](DIFFERENTIAL.md)，不是正式 D 编号。

### 新装默认站点描述不同

- **Rust**：`/api/public` 首次读取缺失的 `description` 时使用
  `"Komari Monitor, a simple server monitoring tool."` 并写回 `configs`。
- **Go**：启动时 `config.GetManyAs[config.Legacy]()` 把 `config.Legacy` 的
  `default:"A simple server monitor tool."` 写入 `configs`，因此
  `/api/public`、`common:getPublicInfo` 和管理端设置读到的是该值。
- **原因**：Rust 的公开设置回退文案沿用了 Go `database.GetPublicInfo` 的
  缺失键回退，但 Go 实际启动时会先按 `config.Legacy` 的 default tag 落库。
- **影响**：全新数据库上 `description` 不同；已有 `description` 行的数据库
  不受影响。差分测试登记为 `DF-001`。
- **迁移**：新装部署若要求与 Go 完全一致，可把 `description` 显式设为
  `"A simple server monitor tool."`；或在 Rust 侧统一默认文案（需业务代码
  变更，B23 未修改）。
- **状态**：差分测试发现，待 B11 编号与确认。

### 启动时配置默认值落库策略不同

- **Rust**：配置默认值按 API 首次读取时惰性写回；全新库启动后
  `GET /api/admin/settings` 只包含已被读取过的键。
- **Go**：启动时 `config.GetManyAs[config.Legacy]()` 把所有带 `default` tag
  的键写入 `configs`，因此管理端设置一开始就有完整键集。
- **原因**：Rust 的 `ConfigRepo::get_as_or` 只在调用点写回；没有启动期全量
  落库步骤。
- **影响**：全新数据库上 Go 多出 `api_key`、`auto_discovery_key`、
  `base_scripts_url`、`ddns_enabled`、`ddns_sync_interval`、`eula_accepted`、
  `expire_notification_enabled`、`expire_notification_lead_days`、
  `login_notification`、`notification_enabled`、`notification_method`、
  `notification_template`、`script_domain`、`send_ip_addr_to_guest`、
  `traffic_limit_percentage` 等行；两边对这些键的读取默认值一致。差分测试
  登记为 `DF-002`。
- **迁移**：依赖管理端返回完整键集的客户端需要自行补默认值；已有库通常
  已包含这些行。
- **状态**：差分测试发现，待 B11 编号与确认。

### `/api/admin/settings` 尾斜杠重定向不同

- **Rust**（axum）：`/api/admin/settings` 和 `/api/admin/settings/` 都直接
  返回 200。
- **Go**（Gin）：`GET /api/admin/settings` 返回 `301` 到
  `/api/admin/settings/`，跟随重定向后才是 200。
- **影响**：不跟随重定向的客户端看到 301 vs 200；跟随重定向的客户端最终
  响应一致。差分测试登记为 `DF-003`。
- **迁移**：客户端统一使用带尾斜杠的路径，或配置跟随重定向。
- **状态**：差分测试发现，待 B11 编号与确认。
