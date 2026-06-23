package client

import (
	"time"

	"github.com/gin-gonic/gin"
	"github.com/Fearless743/komari/database/auditlog"
	"github.com/Fearless743/komari/database/clients"
	"github.com/Fearless743/komari/database/models"
	"github.com/Fearless743/komari/database/tasks"
)

func TaskResult(c *gin.Context) {
	token := c.Query("token")
	clientId, _ := clients.GetClientUUIDByToken(token)
	if clientId == "" {
		c.JSON(400, gin.H{"status": "error", "message": "Invalid or missing token"})
		return
	}
	var req struct {
		TaskId     string    `json:"task_id" binding:"required"`
		Result     string    `json:"result" binding:"required"`
		ExitCode   int       `json:"exit_code"`
		FinishedAt time.Time `json:"finished_at" binding:"required"`
	}
	if err := c.ShouldBindJSON(&req); err != nil {
		c.JSON(400, gin.H{"status": "error", "message": "Invalid request"})
		return
	}

	if err := tasks.SaveTaskResult(req.TaskId, clientId, req.Result, req.ExitCode, models.FromTime(req.FinishedAt)); err != nil {
		c.JSON(500, gin.H{"status": "error", "message": "Failed to update task result: " + err.Error()})
		return
	}
	// 回填命令溯源日志中的退出码与执行结果
	auditlog.UpdateExecResult(req.TaskId, clientId, req.ExitCode, req.Result)

	c.JSON(200, gin.H{"status": "success", "message": "Task result updated successfully"})
}
