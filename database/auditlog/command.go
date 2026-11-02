package auditlog

import (
	"log"
	"time"

	"github.com/Fearless743/komari/database/dbcore"
	"github.com/Fearless743/komari/database/models"
	"gorm.io/gorm"
)

// CommandAuditFilter 描述命令溯源日志的查询过滤条件。
type CommandAuditFilter struct {
	Source     string
	ClientUUID string
	UserUUID   string
	Keyword    string // 模糊匹配命令内容
	StartTime  *time.Time
	EndTime    *time.Time
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
