package admin

import (
	"encoding/json"
	"net/http"
	"strconv"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/komari-monitor/komari/api"
	"github.com/komari-monitor/komari/config"
	"github.com/komari-monitor/komari/database"
	"github.com/komari-monitor/komari/database/clients"
	"github.com/komari-monitor/komari/database/models"
	"github.com/komari-monitor/komari/utils/ddns"
	"github.com/komari-monitor/komari/utils/ddns/factory"
)

func GetDdnsProvider(c *gin.Context) {
	provider := c.Query("provider")
	if provider != "" {
		cfg, err := database.GetDdnsConfigByName(provider)
		if err != nil {
			api.RespondError(c, 404, "Provider not found: "+err.Error())
			return
		}
		api.RespondSuccess(c, cfg)
		return
	}
	providers := factory.GetProviderConfigs()
	if len(providers) == 0 {
		api.RespondError(c, 404, "No DDNS providers found")
		return
	}
	api.RespondSuccess(c, providers)
}

func SetDdnsProvider(c *gin.Context) {
	var ddnsConfig models.DdnsProvider
	if err := c.ShouldBindJSON(&ddnsConfig); err != nil {
		api.RespondError(c, 400, "Invalid configuration: "+err.Error())
		return
	}
	if ddnsConfig.Name == "" {
		api.RespondError(c, 400, "Provider name is required")
		return
	}
	_, exists := factory.GetConstructor(ddnsConfig.Name)
	if !exists {
		api.RespondError(c, 404, "Provider not found: "+ddnsConfig.Name)
		return
	}
	if err := database.SaveDdnsConfig(&ddnsConfig); err != nil {
		api.RespondError(c, 500, "Failed to save DDNS provider configuration: "+err.Error())
		return
	}
	currentProvider, _ := config.GetAs[string](config.DdnsProviderKey, "none")
	if currentProvider == ddnsConfig.Name {
		if err := ddns.LoadProvider(ddnsConfig.Name, ddnsConfig.Addition); err != nil {
			api.RespondError(c, 500, "Failed to load DDNS provider: "+err.Error())
			return
		}
	}
	api.RespondSuccess(c, gin.H{"message": "DDNS provider set successfully"})
}

func SyncDdns(c *gin.Context) {
	allClients, err := clients.GetAllClientBasicInfo()
	if err != nil {
		api.RespondError(c, 500, "Failed to load clients for DDNS sync: "+err.Error())
		return
	}
	ddns.SyncAll(allClients, "admin", true)
	api.RespondSuccess(c, gin.H{"message": "DDNS sync started"})
}

func GetCloudflareZones(c *gin.Context) {
	var req struct {
		Token string `json:"token"`
	}
	if err := c.ShouldBindJSON(&req); err != nil {
		api.RespondError(c, 400, "Invalid request")
		return
	}
	if req.Token == "" {
		api.RespondError(c, 400, "Token is required")
		return
	}

	reqUrl := "https://api.cloudflare.com/client/v4/zones?status=active&per_page=50"
	httpReq, err := http.NewRequest(http.MethodGet, reqUrl, nil)
	if err != nil {
		api.RespondError(c, 500, err.Error())
		return
	}
	httpReq.Header.Set("Authorization", "Bearer "+req.Token)
	httpReq.Header.Set("Content-Type", "application/json")

	client := &http.Client{Timeout: 10 * time.Second}
	resp, err := client.Do(httpReq)
	if err != nil {
		api.RespondError(c, 500, "Failed to connect to Cloudflare API")
		return
	}
	defer resp.Body.Close()

	if resp.StatusCode != 200 {
		api.RespondError(c, 500, "Cloudflare API Error: status "+strconv.Itoa(resp.StatusCode))
		return
	}

	var result struct {
		Success bool `json:"success"`
		Result  []struct {
			ID   string `json:"id"`
			Name string `json:"name"`
		} `json:"result"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		api.RespondError(c, 500, "Failed to parse Cloudflare API response")
		return
	}

	if !result.Success {
		api.RespondError(c, 500, "Cloudflare returned success=false")
		return
	}

	api.RespondSuccess(c, result.Result)
}
