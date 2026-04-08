package ddns

import (
	"encoding/json"
	"fmt"
	"log"
	"strings"
	"sync"
	"time"

	"github.com/komari-monitor/komari/config"
	"github.com/komari-monitor/komari/database"
	"github.com/komari-monitor/komari/database/auditlog"
	"github.com/komari-monitor/komari/database/models"
	"github.com/komari-monitor/komari/utils/ddns/factory"
)

var (
	currentProvider factory.IDdnsProvider
	mu              sync.Mutex
	once            sync.Once
	lastState       sync.Map
)

func init() {
	All()
}

type state struct {
	IPv4 string
	IPv6 string
}

func CurrentProvider() factory.IDdnsProvider {
	mu.Lock()
	defer mu.Unlock()
	return currentProvider
}

func Initialize() {
	once.Do(func() {
		all := factory.GetAllDdnsProviders()
		for _, provider := range all {
			if _, err := database.GetDdnsConfigByName(provider.GetName()); err == nil {
				continue
			}
			config := provider.GetConfiguration()
			configBytes, err := json.Marshal(config)
			if err != nil {
				log.Printf("Failed to marshal config for DDNS provider %s: %v", provider.GetName(), err)
				continue
			}
			if err := database.SaveDdnsConfig(&models.DdnsProvider{
				Name:     provider.GetName(),
				Addition: string(configBytes),
			}); err != nil {
				log.Printf("Failed to save default config for DDNS provider %s: %v", provider.GetName(), err)
			}
		}
	})

	method, _ := config.GetAs[string](config.DdnsProviderKey, "none")
	if method == "" || method == "none" {
		_ = LoadProvider("empty", "{}")
		return
	}

	senderConfig, err := database.GetDdnsConfigByName(method)
	if err != nil {
		_ = LoadProvider("empty", "{}")
		return
	}
	if err := LoadProvider(method, senderConfig.Addition); err != nil {
		log.Printf("Failed to load DDNS provider %s: %v", method, err)
		_ = LoadProvider("empty", "{}")
	}
}

func LoadProvider(name string, addition string) error {
	mu.Lock()
	defer mu.Unlock()
	constructor, exists := factory.GetConstructor(name)
	if !exists {
		return fmt.Errorf("ddns provider not found: %s", name)
	}
	provider := constructor()
	if err := json.Unmarshal([]byte(addition), provider.GetConfiguration()); err != nil {
		return fmt.Errorf("failed to load config for ddns provider %s: %w", name, err)
	}
	if err := provider.Init(); err != nil {
		return err
	}
	if currentProvider != nil {
		_ = currentProvider.Destroy()
	}
	currentProvider = provider
	return nil
}

func SyncAll(allClients []models.Client, triggeredBy string, force bool) {
	enabled, _ := config.GetAs[bool](config.DdnsEnabledKey, false)
	if !enabled && !force {
		return
	}
	if CurrentProvider() == nil {
		return
	}
	for _, client := range allClients {
		_ = SyncClient(client, triggeredBy, force)
	}
}

func SyncClient(client models.Client, triggeredBy string, force bool) error {
	enabled, _ := config.GetAs[bool](config.DdnsEnabledKey, false)
	if !enabled && !force {
		return nil
	}
	provider := CurrentProvider()
	if provider == nil {
		return nil
	}
	ipv4 := strings.TrimSpace(client.IPv4)
	ipv6 := strings.TrimSpace(client.IPv6)
	if client.DdnsEnabled {
		if ipv4 == "" && ipv6 == "" {
			return nil
		}
	} else if !force {
		return nil
	}
	if ipv4 == "" && ipv6 == "" {
		return nil
	}
	newState := state{IPv4: ipv4, IPv6: ipv6}
	if !force {
		if old, ok := lastState.Load(client.UUID); ok {
			if s, ok := old.(state); ok && s == newState {
				return nil
			}
		}
	}
	cfgMap, _ := getProviderConfigMap()
	cfgMap = mergeClientProviderConfig(cfgMap, client)
	ctx := factory.SyncContext{
		IPv4:           ipv4,
		IPv6:           ipv6,
		ClientUUID:     client.UUID,
		ClientName:     client.Name,
		TriggeredBy:    triggeredBy,
		Force:          force,
		ProviderConfig: cfgMap,
	}
	result, err := provider.Sync(ctx)
	if err != nil {
		auditlog.EventLog("error", fmt.Sprintf("DDNS sync failed for %s: %v", client.UUID, err))
		return err
	}
	if result.ResolvedRecordID != "" && strings.TrimSpace(client.DdnsRecordID) == "" {
		if saveErr := database.UpdateClientDdnsRecordID(client.UUID, result.ResolvedRecordID); saveErr != nil {
			log.Printf("Failed to persist resolved DDNS record id for %s: %v", client.UUID, saveErr)
		}
	}
	lastState.Store(client.UUID, newState)
	auditlog.EventLog("info", fmt.Sprintf("DDNS synced for %s at %s", client.UUID, time.Now().Format(time.RFC3339)))
	return nil
}

func getProviderConfigMap() (map[string]any, error) {
	method, _ := config.GetAs[string](config.DdnsProviderKey, "none")
	if method == "" || method == "none" {
		return map[string]any{}, nil
	}
	providerConfig, err := database.GetDdnsConfigByName(method)
	if err != nil {
		return nil, err
	}
	result := map[string]any{}
	if err := json.Unmarshal([]byte(providerConfig.Addition), &result); err != nil {
		return nil, err
	}
	return result, nil
}

func mergeClientProviderConfig(cfg map[string]any, client models.Client) map[string]any {
	result := make(map[string]any, len(cfg)+3)
	for k, v := range cfg {
		result[k] = v
	}
	if client.DdnsHostname != "" {
		result["hostname"] = client.DdnsHostname
	}
	if client.DdnsRecordID != "" {
		result["record_id"] = client.DdnsRecordID
	}
	if client.DdnsRecordType != "" {
		result["record_type"] = client.DdnsRecordType
	}
	return result
}
