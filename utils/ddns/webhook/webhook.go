package webhook

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"github.com/komari-monitor/komari/utils/ddns/factory"
)

type Provider struct {
	Addition
}

type Addition struct {
	URL         string `json:"url" required:"true"`
	Method      string `json:"method" default:"POST" type:"option" options:"POST,PUT,PATCH"`
	ContentType string `json:"content_type" default:"application/json"`
	Headers     string `json:"headers" help:"HTTP headers in JSON format"`
	Body        string `json:"body" type:"richtext" default:"{\"client_uuid\":\"{{client_uuid}}\",\"client_name\":\"{{client_name}}\",\"ipv4\":\"{{ipv4}}\",\"ipv6\":\"{{ipv6}}\",\"triggered_by\":\"{{triggered_by}}\"}"`
}

func (p *Provider) GetName() string                         { return "webhook" }
func (p *Provider) GetConfiguration() factory.Configuration { return &p.Addition }
func (p *Provider) Init() error                             { return nil }
func (p *Provider) Destroy() error                          { return nil }

func (p *Provider) Sync(ctx factory.SyncContext) (factory.SyncResult, error) {
	if strings.TrimSpace(p.URL) == "" {
		return factory.SyncResult{}, fmt.Errorf("webhook URL is not configured")
	}
	method := strings.ToUpper(strings.TrimSpace(p.Method))
	if method == "" {
		method = http.MethodPost
	}
	body := replacePlaceholders(p.Body, ctx)
	req, err := http.NewRequest(method, p.URL, bytes.NewBufferString(body))
	if err != nil {
		return factory.SyncResult{}, err
	}
	contentType := strings.TrimSpace(p.ContentType)
	if contentType == "" {
		contentType = "application/json"
	}
	req.Header.Set("Content-Type", contentType)
	if strings.TrimSpace(p.Headers) != "" {
		var headers map[string]string
		if err := json.Unmarshal([]byte(p.Headers), &headers); err != nil {
			return factory.SyncResult{}, fmt.Errorf("invalid headers json: %w", err)
		}
		for k, v := range headers {
			req.Header.Set(k, v)
		}
	}
	resp, err := (&http.Client{Timeout: 30 * time.Second}).Do(req)
	if err != nil {
		return factory.SyncResult{}, err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		body, _ := io.ReadAll(resp.Body)
		return factory.SyncResult{}, fmt.Errorf("webhook request failed with status %d: %s", resp.StatusCode, string(body))
	}
	return factory.SyncResult{}, nil
}

func replacePlaceholders(template string, ctx factory.SyncContext) string {
	replacer := strings.NewReplacer(
		"{{ipv4}}", ctx.IPv4,
		"{{ipv6}}", ctx.IPv6,
		"{{client_uuid}}", ctx.ClientUUID,
		"{{client_name}}", ctx.ClientName,
		"{{triggered_by}}", ctx.TriggeredBy,
	)
	return replacer.Replace(template)
}

func init() {
	factory.RegisterDdnsProvider(func() factory.IDdnsProvider {
		return &Provider{}
	})
}
