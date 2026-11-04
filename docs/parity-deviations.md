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
    - `clients[]` 的字段集合由 `komari-db::models::Client` 的 serde 规则决定，
      `skip_serializing_if` 会省略空字符串字段；Go 的 `json.Marshal` 会保留全部
      字段（空字符串输出 `""`）；
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

## D-003 旧数据库历史迁移不自动执行

- **Rust**：`komari-db` 的 `migrate()` 只做加法式 schema 迁移、缺失列补齐和
  legacy `configs` 转换；不执行以下 Go 历史迁移：
  - pre-1.0.2 将 OAuth/通知设置从旧 `configs` 列搬出的迁移；
  - `client_infos` 合并到 `clients`；
  - MySQL 路径（Go 的 MySQL 分支同样被注释，但 Rust 会显式返回
    `UnsupportedDatabase`）。
- **Go**：启动时 `AutoMigrate` 会执行上述历史迁移。
- **原因**：这些迁移只影响很老的数据库，Rust 选择不自动改写用户数据，降低误迁移
  风险。
- **影响**：极老数据库直接用 Rust 打开时，可能缺少 OAuth/通知配置或旧客户端信息。
- **迁移**：先用 Go 版启动一次并正常退出，让 Go 完成历史迁移；备份后再切换 Rust。
  参见 [MIGRATION.md](MIGRATION.md) 第 4 节。
- **状态**：兼容性限制，已记录在 `crates/komari-db/README.md`。

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
