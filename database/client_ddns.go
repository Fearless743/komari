package database

import (
	"time"

	"github.com/komari-monitor/komari/database/dbcore"
	"github.com/komari-monitor/komari/database/models"
)

func UpdateClientDdnsRecordID(uuid string, recordID string) error {
	return dbcore.GetDBInstance().Model(&models.Client{}).Where("uuid = ?", uuid).Updates(map[string]any{
		"ddns_record_id": recordID,
		"updated_at":     time.Now(),
	}).Error
}
