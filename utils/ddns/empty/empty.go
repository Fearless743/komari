package empty

import "github.com/komari-monitor/komari/utils/ddns/factory"

type Provider struct{}

type Addition struct{}

func (p *Provider) GetName() string                         { return "empty" }
func (p *Provider) GetConfiguration() factory.Configuration { return &Addition{} }
func (p *Provider) Init() error                             { return nil }
func (p *Provider) Destroy() error                          { return nil }
func (p *Provider) Sync(ctx factory.SyncContext) (factory.SyncResult, error) {
	return factory.SyncResult{}, nil
}

func init() {
	factory.RegisterDdnsProvider(func() factory.IDdnsProvider {
		return &Provider{}
	})
}
