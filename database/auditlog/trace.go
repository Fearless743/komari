package auditlog

import (
	"log"
	"time"

	"github.com/komari-monitor/komari/database/dbcore"
	"github.com/komari-monitor/komari/database/models"
)

// RecordTerminalOpen 记录一次终端会话的建立。
func RecordTerminalOpen(sessionID, actorUUID, actorIP, clientUUID string) {
	createTrace(&models.AuditTrace{
		Category:   models.AuditCategoryTerminal,
		Action:     models.AuditActionOpen,
		SessionID:  sessionID,
		ActorUUID:  actorUUID,
		ActorIP:    actorIP,
		ClientUUID: clientUUID,
		Time:       models.FromTime(time.Now()),
	})
}

// RecordTerminalClose 记录终端会话关闭，并写入会话时长（毫秒）。
func RecordTerminalClose(sessionID, actorUUID, actorIP, clientUUID string, duration time.Duration) {
	RecordTerminalCloseWithDetail(sessionID, actorUUID, actorIP, clientUUID, duration, "")
}

// RecordTerminalCloseWithDetail 在记录关闭的同时附带备注（如会话录像文件路径）。
func RecordTerminalCloseWithDetail(sessionID, actorUUID, actorIP, clientUUID string, duration time.Duration, detail string) {
	ms := duration.Milliseconds()
	createTrace(&models.AuditTrace{
		Category:   models.AuditCategoryTerminal,
		Action:     models.AuditActionClose,
		SessionID:  sessionID,
		ActorUUID:  actorUUID,
		ActorIP:    actorIP,
		ClientUUID: clientUUID,
		DurationMs: &ms,
		Detail:     detail,
		Time:       models.FromTime(time.Now()),
	})
}

// RecordTerminalTimeout 记录终端会话因被控端未连接而超时。
func RecordTerminalTimeout(sessionID, actorUUID, actorIP, clientUUID string) {
	createTrace(&models.AuditTrace{
		Category:   models.AuditCategoryTerminal,
		Action:     models.AuditActionTimeout,
		SessionID:  sessionID,
		ActorUUID:  actorUUID,
		ActorIP:    actorIP,
		ClientUUID: clientUUID,
		Detail:     "agent connection timeout",
		Time:       models.FromTime(time.Now()),
	})
}

// RecordExecDispatch 为一次远程执行任务的每个目标客户端各记录一条下发记录。
func RecordExecDispatch(taskID, actorUUID, actorIP, command string, clients []string) {
	for _, clientUUID := range clients {
		createTrace(&models.AuditTrace{
			Category:   models.AuditCategoryExec,
			Action:     models.AuditActionDispatch,
			SessionID:  taskID,
			ActorUUID:  actorUUID,
			ActorIP:    actorIP,
			ClientUUID: clientUUID,
			Command:    command,
			Time:       models.FromTime(time.Now()),
		})
	}
}

// RecordExecResult 记录某个客户端回传的远程执行结果。
func RecordExecResult(taskID, clientUUID, command, result string, exitCode int) {
	code := exitCode
	createTrace(&models.AuditTrace{
		Category:   models.AuditCategoryExec,
		Action:     models.AuditActionResult,
		SessionID:  taskID,
		ClientUUID: clientUUID,
		Command:    command,
		Detail:     result,
		ExitCode:   &code,
		Time:       models.FromTime(time.Now()),
	})
}

func createTrace(t *models.AuditTrace) {
	db := dbcore.GetDBInstance()
	if err := db.Create(t).Error; err != nil {
		log.Println("Failed to write audit trace:", err)
	}
}

// AuditTraceFilter 描述审计溯源查询的过滤条件。
type AuditTraceFilter struct {
	Category   string
	Action     string
	ClientUUID string
	ActorUUID  string
	SessionID  string
	Keyword    string // 模糊匹配 command / detail
	StartTime  *time.Time
	EndTime    *time.Time
}

// QueryAuditTraces 按过滤条件分页查询审计溯源记录，返回记录与总数。
// limit <= 0 表示不分页（用于导出）。
func QueryAuditTraces(f AuditTraceFilter, limit, offset int) ([]models.AuditTrace, int64, error) {
	db := dbcore.GetDBInstance().Model(&models.AuditTrace{})

	if f.Category != "" {
		db = db.Where("category = ?", f.Category)
	}
	if f.Action != "" {
		db = db.Where("action = ?", f.Action)
	}
	if f.ClientUUID != "" {
		db = db.Where("client_uuid = ?", f.ClientUUID)
	}
	if f.ActorUUID != "" {
		db = db.Where("actor_uuid = ?", f.ActorUUID)
	}
	if f.SessionID != "" {
		db = db.Where("session_id = ?", f.SessionID)
	}
	if f.Keyword != "" {
		like := "%" + f.Keyword + "%"
		db = db.Where("command LIKE ? OR detail LIKE ?", like, like)
	}
	if f.StartTime != nil {
		db = db.Where("time >= ?", *f.StartTime)
	}
	if f.EndTime != nil {
		db = db.Where("time <= ?", *f.EndTime)
	}

	var total int64
	if err := db.Count(&total).Error; err != nil {
		return nil, 0, err
	}

	query := db.Order("time desc")
	if limit > 0 {
		query = query.Limit(limit).Offset(offset)
	}

	var traces []models.AuditTrace
	if err := query.Find(&traces).Error; err != nil {
		return nil, 0, err
	}
	return traces, total, nil
}

// RemoveOldAuditTraces 删除超过指定天数的审计溯源记录。
func RemoveOldAuditTraces(days int) {
	if days <= 0 {
		return
	}
	db := dbcore.GetDBInstance()
	threshold := time.Now().AddDate(0, 0, -days)
	if err := db.Where("time < ?", threshold).Delete(&models.AuditTrace{}).Error; err != nil {
		log.Println("Failed to remove old audit traces:", err)
	}
}
