package factory

import "github.com/komari-monitor/komari/utils/item"

type IDdnsProvider interface {
	GetName() string
	GetConfiguration() Configuration
	Sync(ctx SyncContext) (SyncResult, error)
	Init() error
	Destroy() error
}

type Configuration interface{}

type DdnsConstructor func() IDdnsProvider

type SyncContext struct {
	IPv4           string         `json:"ipv4"`
	IPv6           string         `json:"ipv6"`
	ClientUUID     string         `json:"client_uuid"`
	ClientName     string         `json:"client_name"`
	TriggeredBy    string         `json:"triggered_by"`
	Force          bool           `json:"force"`
	ProviderConfig map[string]any `json:"provider_config,omitempty"`
}

type SyncResult struct {
	ResolvedRecordID string `json:"resolved_record_id,omitempty"`
}

func GetItems(config Configuration) []item.Item {
	return item.Parse(config)
}
