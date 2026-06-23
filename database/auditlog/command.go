package auditlog

import (
	"log"
	"time"

	"github.com/Fearless743/komari/database/dbcore"
	"github.com/Fearless743/komari/database/models"
	"gorm.io/gorm"
)

const (
	CommandSourceTerminal = "terminal"
	CommandSourceExec     = "exec"

	// MaxCommandOutputBytes 单条命令记录保存的最大输出字节数，超出截断，避免单行过大。
	MaxCommandOutputBytes = 64 * 1024
)

// TruncateOutput 将输出裁剪到最大长度，并在被截断时追加提示。
func TruncateOutput(output string) string {
	if len(output) <= MaxCommandOutputBytes {
		return output
	}
	return output[:MaxCommandOutputBytes] + "\n...[output truncated]"
}

// CommandAuditFilter 描述命令溯源日志的查询过滤条件。
type CommandAuditFilter struct {
	Source     string
	ClientUUID string
	UserUUID   string
	Keyword    string // 模糊匹配命令内容
	StartTime  *time.Time
	EndTime    *time.Time
}

// RecordTerminalCommand 记录一条终端会话中执行的命令，返回新建记录的 ID，
// 以便随后通过 UpdateTerminalOutput 回填该命令运行期间产生的输出。
func RecordTerminalCommand(userUUID, userIP, clientUUID, sessionID, command string) uint {
	entry := &models.CommandAudit{
		Source:     CommandSourceTerminal,
		UserUUID:   userUUID,
		UserIP:     userIP,
		ClientUUID: clientUUID,
		SessionID:  sessionID,
		Command:    command,
	}
	createCommandAudit(entry)
	return entry.ID
}

// UpdateTerminalOutput 回填某条终端命令记录的输出（命令运行期间的终端回显流）。
func UpdateTerminalOutput(id uint, output string) {
	if id == 0 {
		return
	}
	db := dbcore.GetDBInstance()
	if err := db.Model(&models.CommandAudit{}).
		Where("id = ?", id).
		Update("output", TruncateOutput(output)).Error; err != nil {
		log.Println("Failed to update terminal command output:", err)
	}
}

// RecordExecCommand 记录一条远程执行（REC）下发到某个被控端的命令。
func RecordExecCommand(userUUID, userIP, clientUUID, taskID, command string) {
	createCommandAudit(&models.CommandAudit{
		Source:     CommandSourceExec,
		UserUUID:   userUUID,
		UserIP:     userIP,
		ClientUUID: clientUUID,
		SessionID:  taskID,
		Command:    command,
	})
}

// UpdateExecResult 在被控端回报远程执行结果后，回填对应命令记录的退出码与输出结果。
func UpdateExecResult(taskID, clientUUID string, exitCode int, output string) {
	db := dbcore.GetDBInstance()
	if err := db.Model(&models.CommandAudit{}).
		Where("source = ? AND session_id = ? AND client_uuid = ?", CommandSourceExec, taskID, clientUUID).
		Updates(map[string]interface{}{
			"exit_code": exitCode,
			"output":    TruncateOutput(output),
		}).Error; err != nil {
		log.Println("Failed to update command audit result:", err)
	}
}

func createCommandAudit(entry *models.CommandAudit) {
	entry.Time = models.FromTime(time.Now())
	if err := dbcore.GetDBInstance().Create(entry).Error; err != nil {
		log.Println("Failed to write command audit log:", err)
	}
}

// QueryCommandAudits 按过滤条件分页查询命令溯源日志，返回当前页数据与符合条件的总数。
func QueryCommandAudits(filter CommandAuditFilter, page, limit int) ([]models.CommandAudit, int64, error) {
	if page <= 0 {
		page = 1
	}
	if limit <= 0 {
		limit = 100
	}
	query := buildCommandAuditQuery(filter)

	var total int64
	if err := query.Count(&total).Error; err != nil {
		return nil, 0, err
	}

	var logs []models.CommandAudit
	offset := (page - 1) * limit
	if err := query.Order("time desc").Limit(limit).Offset(offset).Find(&logs).Error; err != nil {
		return nil, 0, err
	}
	return logs, total, nil
}

// ExportCommandAudits 返回符合过滤条件的全部命令溯源日志，用于导出。
func ExportCommandAudits(filter CommandAuditFilter) ([]models.CommandAudit, error) {
	var logs []models.CommandAudit
	if err := buildCommandAuditQuery(filter).Order("time desc").Find(&logs).Error; err != nil {
		return nil, err
	}
	return logs, nil
}

func buildCommandAuditQuery(filter CommandAuditFilter) *gorm.DB {
	db := dbcore.GetDBInstance().Model(&models.CommandAudit{})
	if filter.Source != "" {
		db = db.Where("source = ?", filter.Source)
	}
	if filter.ClientUUID != "" {
		db = db.Where("client_uuid = ?", filter.ClientUUID)
	}
	if filter.UserUUID != "" {
		db = db.Where("user_uuid = ?", filter.UserUUID)
	}
	if filter.Keyword != "" {
		db = db.Where("command LIKE ?", "%"+filter.Keyword+"%")
	}
	if filter.StartTime != nil {
		db = db.Where("time >= ?", *filter.StartTime)
	}
	if filter.EndTime != nil {
		db = db.Where("time <= ?", *filter.EndTime)
	}
	return db
}

// RemoveOldCommandAudits 删除超过 90 天的命令溯源日志。
func RemoveOldCommandAudits() {
	db := dbcore.GetDBInstance()
	threshold := time.Now().AddDate(0, 0, -90)
	if err := db.Where("time < ?", threshold).Delete(&models.CommandAudit{}).Error; err != nil {
		log.Println("Failed to remove old command audit logs:", err)
	}
}
