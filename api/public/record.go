package public

import (
	"strconv"
	"time"

	"github.com/Fearless743/komari/api"
	"github.com/Fearless743/komari/database/accounts"
	"github.com/Fearless743/komari/database/dbcore"
	"github.com/Fearless743/komari/database/models"
	records "github.com/Fearless743/komari/database/records"
	"github.com/gin-gonic/gin"
)

func GetRecordsByUUID(c *gin.Context) {
	uuid := c.Query("uuid")
	loadType := c.Query("load_type")

	// 登录状态检查
	isLogin := false
	session, _ := c.Cookie("session_token")
	_, err := accounts.GetUserBySession(session)
	if err == nil {
		isLogin = true
	}

	// 仅在未登录时需要 Hidden 信息做过滤
	hiddenMap := map[string]bool{}
	if !isLogin {
		var hiddenClients []models.Client
		db := dbcore.GetDBInstance()
		_ = db.Select("uuid").Where("hidden = ?", true).Find(&hiddenClients).Error
		for _, cli := range hiddenClients {
			hiddenMap[cli.UUID] = true
		}

		if hiddenMap[uuid] {
			api.RespondError(c, 400, "UUID is required") //防止未登录用户获取隐藏客户端数据
			return
		}
	}

	hours := c.Query("hours")
	if uuid == "" {
		api.RespondError(c, 400, "UUID is required")
		return
	}
	if hours == "" {
		hours = "4"
	}

	hoursInt, err := strconv.Atoi(hours)
	if err != nil {
		api.RespondError(c, 400, "Invalid hours parameter")
		return
	}

	// 验证 load_type 参数
	validLoadTypes := map[string]bool{
		"cpu": true, "ram": true, "swap": true,
		"load": true, "temp": true, "disk": true, "network": true,
		"process": true, "connections": true, "all": true, "": true,
	}

	if !validLoadTypes[loadType] {
		api.RespondError(c, 400, "Invalid load_type parameter")
		return
	}

	clientRecords, err := records.GetRecordsByClientAndTime(uuid, time.Now().Add(-time.Duration(hoursInt)*time.Hour), time.Now())
	if err != nil {
		api.RespondError(c, 500, "Failed to fetch records: "+err.Error())
		return
	}

	// 准备基本响应
	response := gin.H{
		"records": clientRecords,
		"count":   len(clientRecords),
	}

	// 如果有load_type过滤，应用过滤
	if loadType != "" && loadType != "all" {
		filteredRecords := filterRecordsByLoadType(clientRecords, loadType)
		response = gin.H{
			"records":   filteredRecords,
			"count":     len(filteredRecords),
			"load_type": loadType,
		}
	}

	// 自动检测是否有GPU数据并附加到响应中
	if loadType == "" || loadType == "all" || loadType == "gpu" {
		gpuRecords, err := records.GetGPURecordsByClientAndTime(uuid, time.Now().Add(-time.Duration(hoursInt)*time.Hour), time.Now())
		if err == nil && len(gpuRecords) > 0 {
			// 按设备索引分组数据，构建简化的设备结构
			gpuDevices := make(map[string]interface{})

			for _, record := range gpuRecords {
				deviceKey := strconv.Itoa(record.DeviceIndex)

				// 如果设备还没有初始化，创建设备信息
				if gpuDevices[deviceKey] == nil {
					gpuDevices[deviceKey] = gin.H{
						"device_index": record.DeviceIndex,
						"device_name":  record.DeviceName,
						"records":      []models.GPURecord{},
					}
				}

				// 添加记录到设备
				device := gpuDevices[deviceKey].(gin.H)
				records := device["records"].([]models.GPURecord)
				device["records"] = append(records, record)
				gpuDevices[deviceKey] = device
			}

			// 添加优化后的GPU数据结构到响应
			response["gpu_devices"] = gpuDevices
			response["has_gpu_data"] = true
		} else {
			response["has_gpu_data"] = false
		}
	}

	api.RespondSuccess(c, response)
}

// filterRecordsByLoadType 根据 load_type 过滤记录，只返回相关字段
func filterRecordsByLoadType(records []models.Record, loadType string) []gin.H {
	filteredRecords := make([]gin.H, 0, len(records))

	for _, r := range records {
		record := gin.H{
			"client": r.Client,
			"time":   r.Time,
		}

		switch loadType {
		case "cpu":
			record["cpu"] = r.Cpu
		case "gpu":
			record["gpu"] = r.Gpu
		case "ram":
			record["ram"] = r.Ram
			record["ram_total"] = r.RamTotal
			if r.RamTotal > 0 {
				record["ram_percent"] = float32(r.Ram) / float32(r.RamTotal) * 100
			}
		case "swap":
			record["swap"] = r.Swap
			record["swap_total"] = r.SwapTotal
			if r.SwapTotal > 0 {
				record["swap_percent"] = float32(r.Swap) / float32(r.SwapTotal) * 100
			}
		case "load":
			record["load"] = r.Load
		case "temp":
			record["temp"] = r.Temp
		case "disk":
			record["disk"] = r.Disk
			record["disk_total"] = r.DiskTotal
			if r.DiskTotal > 0 {
				record["disk_percent"] = float32(r.Disk) / float32(r.DiskTotal) * 100
			}
		case "network":
			record["net_in"] = r.NetIn
			record["net_out"] = r.NetOut
			record["net_total_up"] = r.NetTotalUp
			record["net_total_down"] = r.NetTotalDown
		case "process":
			record["process"] = r.Process
		case "connections":
			record["connections"] = r.Connections
			record["connections_udp"] = r.ConnectionsUdp
			record["connections_tcp"] = r.Connections - r.ConnectionsUdp
		}

		filteredRecords = append(filteredRecords, record)
	}

	return filteredRecords
}
