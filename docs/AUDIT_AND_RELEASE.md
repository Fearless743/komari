# 日志溯源（终端 / 远程执行）与发布流程

## 一、日志溯源功能

为「终端系统」与「远程执行」新增了结构化的审计溯源能力，独立于通用 `logs`
（自由文本）之外，便于按客户端 / 操作者 / 时间范围检索并导出。

### 数据模型

`AuditTrace`（表 `audit_traces`，见 `database/models/audit_trace.go`）：

| 字段          | 说明                                                   |
| ------------- | ------------------------------------------------------ |
| `category`    | `terminal`（终端会话）/ `exec`（远程执行）             |
| `action`      | 终端：`open` / `close` / `timeout`；执行：`dispatch` / `result` |
| `session_id`  | 终端会话 id 或远程执行 task id（串联同一会话/任务）    |
| `actor_uuid` / `actor_ip` | 发起操作的用户与来源 IP                   |
| `client_uuid` | 被操作的目标客户端                                     |
| `command`     | 远程执行的命令内容                                     |
| `detail`      | 输出 / 备注（命令结果、离线提示等）                    |
| `exit_code`   | 远程执行退出码                                         |
| `duration_ms` | 终端会话时长（毫秒）                                   |
| `time`        | 记录时间                                               |

### 埋点位置

- 终端：`web/api/terminal/forward.go`（open / close + 时长）、
  `web/api/terminal/request.go`（timeout）。
- 远程执行：`web/rpc/jsonrpc/admin.system.go`（命令下发 `dispatch`、离线客户端结果）、
  `web/rpc/jsonrpc/client.go`（客户端回传结果 `result`）。

> 说明：终端为原始字节流，未对键击 / 输出做内容级录制（不可靠且涉及隐私）；
> 溯源粒度为「谁、在何时、对哪个客户端、建立/关闭了会话、持续多久」。

### 接口

- 查询（分页 + 过滤）：`GET /api/admin/audit`
  参数：`category` `action` `client_uuid` `actor_uuid` `session_id`
  `keyword` `start_time` `end_time` `limit` `page`。
- 导出：`GET /api/admin/audit/export`（同一组过滤参数 + `format=csv|json`，默认 csv）。
  CSV 带 UTF-8 BOM，便于 Excel 正确显示中文。

前端页面：`/admin/audit`（komari-web `src/pages/admin/audit.tsx`），支持筛选、
分页、导出 CSV / JSON。

保留策略：`cmd/server.go` 的定时清理任务会删除 90 天前的溯源记录
（`auditlog.RemoveOldAuditTraces`）。

## 二、发布流程（前端先行）

后端二进制通过 `go:embed` 内嵌前端构建产物（`web/public/defaultTheme/dist`），
因此 **必须先成功构建前端，后端才能构建/发布**。

CI（`.github/workflows/{build,release,docker-publish,development}.yml`）已保证该顺序：

1. clone `Fearless743/komari-web` 并 `npm install && npm run build`；
2. 校验前端产物存在（`index.html`），缺失则 `exit 1` 直接中止后端构建/发布；
3. 将产物拷贝至 `web/public/defaultTheme/dist` 后再编译 Go 后端。

### 人工发布顺序

1. 先在 `Fearless743/komari-web` 合并/发布前端改动；
2. 确认前端可成功 `npm run build`；
3. 再在 `Fearless743/komari` 创建后端 Release（`release.yml` 会重新拉取并构建最新前端，
   构建失败即中止，从而保证「前端先行且构建成功」）。
