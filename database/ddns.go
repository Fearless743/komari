package database

import (
	"github.com/komari-monitor/komari/database/dbcore"
	"github.com/komari-monitor/komari/database/models"
)

func GetAllDdnsConfigs() []models.DdnsProvider {
	db := dbcore.GetDBInstance()
	var result []models.DdnsProvider
	if err := db.Find(&result).Error; err != nil {
		return nil
	}
	return result
}

func GetDdnsConfigByName(name string) (*models.DdnsProvider, error) {
	db := dbcore.GetDBInstance()
	var config models.DdnsProvider
	if err := db.Where("name = ?", name).First(&config).Error; err != nil {
		return nil, err
	}
	return &config, nil
}

func SaveDdnsConfig(config *models.DdnsProvider) error {
	db := dbcore.GetDBInstance()
	return db.Save(config).Error
}
