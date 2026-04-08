package ddns

import (
	_ "github.com/komari-monitor/komari/utils/ddns/cloudflare"
	_ "github.com/komari-monitor/komari/utils/ddns/empty"
	_ "github.com/komari-monitor/komari/utils/ddns/webhook"
)

func All() {
	// empty function to ensure all DDNS providers are registered
}
