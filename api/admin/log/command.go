package log

import (
	"encoding/csv"
	"fmt"
	"strconv"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/Fearless743/komari/api"
	"github.com/Fearless743/komari/database/auditlog"
)

// parseCommandAuditFilter 从查询参数构建命令溯源日志的过滤条件。
func parseCommandAuditFilter(c *gin.Context) auditlog.CommandAuditFilter {
	filter := auditlog.CommandAuditFilter{
		Source:     c.Query("source"),
		ClientUUID: c.Query("client_uuid"),
		UserUUID:   c.Query("user_uuid"),
		Keyword:    c.Query("keyword"),
	}
	if start := c.Query("start"); start != "" {
		if t, err := time.Parse(time.RFC3339, start); err == nil {
			filter.StartTime = &t
		}
	}
	if end := c.Query("end"); end != "" {
		if t, err := time.Parse(time.RFC3339, end); err == nil {
			filter.EndTime = &t
		}
	}
	return filter
}

// GetCommandLogs 分页查询终端 / 远程执行的命令溯源日志。
func GetCommandLogs(c *gin.Context) {
	limit, err := strconv.Atoi(c.DefaultQuery("limit", "20"))
	if err != nil || limit <= 0 {
		api.RespondError(c, 400, "Invalid limit")
		return
	}
	page, err := strconv.Atoi(c.DefaultQuery("page", "1"))
	if err != nil || page <= 0 {
		api.RespondError(c, 400, "Invalid page")
		return
	}

	logs, total, err := auditlog.QueryCommandAudits(parseCommandAuditFilter(c), page, limit)
	if err != nil {
		api.RespondError(c, 500, "Failed to retrieve command logs: "+err.Error())
		return
	}
	api.RespondSuccess(c, gin.H{"logs": logs, "total": total})
}

// ExportCommandLogs 将符合过滤条件的命令溯源日志导出为 CSV 或 JSON 文件。
func ExportCommandLogs(c *gin.Context) {
	logs, err := auditlog.ExportCommandAudits(parseCommandAuditFilter(c))
	if err != nil {
		api.RespondError(c, 500, "Failed to export command logs: "+err.Error())
		return
	}

	format := c.DefaultQuery("format", "csv")
	filename := fmt.Sprintf("command-audit-%s", time.Now().Format("20060102-150405"))

	if format == "json" {
		c.Header("Content-Disposition", fmt.Sprintf("attachment; filename=\"%s.json\"", filename))
		c.JSON(200, logs)
		return
	}

	// 默认导出 CSV
	c.Header("Content-Type", "text/csv; charset=utf-8")
	c.Header("Content-Disposition", fmt.Sprintf("attachment; filename=\"%s.csv\"", filename))
	// 写入 UTF-8 BOM，方便 Excel 正确识别中文
	c.Writer.Write([]byte{0xEF, 0xBB, 0xBF})

	w := csv.NewWriter(c.Writer)
	defer w.Flush()
	w.Write([]string{"id", "time", "source", "user_uuid", "user_ip", "client_uuid", "session_id", "exit_code", "command", "output"})
	for _, l := range logs {
		exitCode := ""
		if l.ExitCode != nil {
			exitCode = strconv.Itoa(*l.ExitCode)
		}
		w.Write([]string{
			strconv.FormatUint(uint64(l.ID), 10),
			time.Time(l.Time).Format(time.RFC3339),
			l.Source,
			l.UserUUID,
			l.UserIP,
			l.ClientUUID,
			l.SessionID,
			exitCode,
			l.Command,
			l.Output,
		})
	}
}
