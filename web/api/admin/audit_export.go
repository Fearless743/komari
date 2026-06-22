package admin

import (
	"encoding/csv"
	"encoding/json"
	"fmt"
	"net/http"
	"strconv"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/komari-monitor/komari/database/auditlog"
)

// ExportAuditTraces 以 CSV 或 JSON 形式导出终端 / 远程执行的日志溯源记录。
//
// 查询参数（全部可选）：
//
//	format      csv | json（默认 csv）
//	category    terminal | exec
//	action      open | close | timeout | dispatch | result
//	client_uuid 目标客户端 UUID
//	actor_uuid  操作者 UUID
//	session_id  终端会话 id / 远程执行 task id
//	keyword     模糊匹配命令或输出
//	start_time / end_time  RFC3339 或 "2006-01-02 15:04:05"
func ExportAuditTraces(c *gin.Context) {
	filter := auditlog.AuditTraceFilter{
		Category:   c.Query("category"),
		Action:     c.Query("action"),
		ClientUUID: c.Query("client_uuid"),
		ActorUUID:  c.Query("actor_uuid"),
		SessionID:  c.Query("session_id"),
		Keyword:    c.Query("keyword"),
	}
	if s := c.Query("start_time"); s != "" {
		t, err := parseExportTime(s)
		if err != nil {
			c.JSON(http.StatusBadRequest, gin.H{"status": "error", "message": "Invalid start_time: " + s})
			return
		}
		filter.StartTime = &t
	}
	if s := c.Query("end_time"); s != "" {
		t, err := parseExportTime(s)
		if err != nil {
			c.JSON(http.StatusBadRequest, gin.H{"status": "error", "message": "Invalid end_time: " + s})
			return
		}
		filter.EndTime = &t
	}

	// 导出全部匹配记录（不分页）。
	traces, _, err := auditlog.QueryAuditTraces(filter, 0, 0)
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"status": "error", "message": "Failed to query audit traces: " + err.Error()})
		return
	}

	stamp := time.Now().Format("20060102-150405")
	if c.Query("format") == "json" {
		c.Header("Content-Disposition", fmt.Sprintf("attachment; filename=audit-traces-%s.json", stamp))
		c.Header("Content-Type", "application/json; charset=utf-8")
		enc := json.NewEncoder(c.Writer)
		enc.SetIndent("", "  ")
		_ = enc.Encode(traces)
		return
	}

	c.Header("Content-Disposition", fmt.Sprintf("attachment; filename=audit-traces-%s.csv", stamp))
	c.Header("Content-Type", "text/csv; charset=utf-8")
	// 写入 UTF-8 BOM，便于 Excel 正确识别中文。
	_, _ = c.Writer.Write([]byte{0xEF, 0xBB, 0xBF})

	w := csv.NewWriter(c.Writer)
	defer w.Flush()
	_ = w.Write([]string{"id", "time", "category", "action", "session_id", "actor_uuid", "actor_ip", "client_uuid", "command", "exit_code", "duration_ms", "detail"})
	for _, t := range traces {
		_ = w.Write([]string{
			strconv.FormatUint(uint64(t.ID), 10),
			time.Time(t.Time).Format(time.RFC3339),
			t.Category,
			t.Action,
			t.SessionID,
			t.ActorUUID,
			t.ActorIP,
			t.ClientUUID,
			t.Command,
			intPtrToString(t.ExitCode),
			int64PtrToString(t.DurationMs),
			t.Detail,
		})
	}
}

func parseExportTime(s string) (time.Time, error) {
	if t, err := time.Parse(time.RFC3339, s); err == nil {
		return t, nil
	}
	return time.ParseInLocation("2006-01-02 15:04:05", s, time.Local)
}

func intPtrToString(p *int) string {
	if p == nil {
		return ""
	}
	return strconv.Itoa(*p)
}

func int64PtrToString(p *int64) string {
	if p == nil {
		return ""
	}
	return strconv.FormatInt(*p, 10)
}
