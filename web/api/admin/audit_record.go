package admin

import (
	"net/http"
	"os"

	"github.com/gin-gonic/gin"
	"github.com/komari-monitor/komari/web/api/terminal"
)

// DownloadTerminalRecord 下载某个终端会话的完整录像（asciicast v2 .cast 文件）。
//
// 查询参数：session_id —— 终端会话 id（即审计溯源中的 session_id）。
// 录像可用 asciinema 等工具回放。
func DownloadTerminalRecord(c *gin.Context) {
	sessionID := c.Query("session_id")
	if sessionID == "" {
		c.JSON(http.StatusBadRequest, gin.H{"status": "error", "message": "session_id is required"})
		return
	}
	path := terminal.RecordPath(sessionID)
	if _, err := os.Stat(path); err != nil {
		c.JSON(http.StatusNotFound, gin.H{"status": "error", "message": "recording not found"})
		return
	}
	c.Header("Content-Type", "application/x-asciicast")
	c.FileAttachment(path, sessionID+".cast")
}
