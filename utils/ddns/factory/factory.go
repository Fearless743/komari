package factory

import "log"

var (
	providers           = make(map[string]IDdnsProvider)
	providerConstructor = make(map[string]DdnsConstructor)
	providerConfigItems = make(map[string]any)
)

func RegisterDdnsProvider(constructor DdnsConstructor) {
	provider := constructor()
	if provider == nil {
		panic("DDNS provider constructor returned nil")
	}
	providerConstructor[provider.GetName()] = constructor
	if _, exists := providers[provider.GetName()]; exists {
		log.Println("DDNS provider already registered: " + provider.GetName())
	}
	providers[provider.GetName()] = provider
	providerConfigItems[provider.GetName()] = GetItems(provider.GetConfiguration())
}

func GetProviderConfigs() map[string]any {
	return providerConfigItems
}

func GetAllDdnsProviders() map[string]IDdnsProvider {
	return providers
}

func GetConstructor(name string) (DdnsConstructor, bool) {
	constructor, exists := providerConstructor[name]
	return constructor, exists
}
