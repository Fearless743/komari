package cloudflare

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/komari-monitor/komari/utils/ddns/factory"
)

var cloudflareAPIBaseURL = "https://api.cloudflare.com/client/v4"

type Provider struct {
	Addition
}

type Addition struct {
	APIToken   string `json:"api_token" required:"true"`
	ZoneID     string `json:"zone_id" required:"true"`
	RecordID   string `json:"record_id" help:"Optional global fallback DNS record ID"`
	RecordType string `json:"record_type" default:"A" type:"option" options:"A,AAAA"`
	TTL        int    `json:"ttl" default:"1" help:"1 means automatic in Cloudflare"`
	Proxied    bool   `json:"proxied" default:"false"`
}

func (p *Provider) GetName() string                         { return "cloudflare" }
func (p *Provider) GetConfiguration() factory.Configuration { return &p.Addition }
func (p *Provider) Init() error                             { return nil }
func (p *Provider) Destroy() error                          { return nil }

func (p *Provider) Sync(ctx factory.SyncContext) (factory.SyncResult, error) {
	if strings.TrimSpace(p.APIToken) == "" || strings.TrimSpace(p.ZoneID) == "" {
		return factory.SyncResult{}, fmt.Errorf("cloudflare DDNS is not fully configured")
	}
	recordType := strings.ToUpper(strings.TrimSpace(p.RecordType))
	if rt, ok := ctx.ProviderConfig["record_type"].(string); ok {
		rt = strings.ToUpper(strings.TrimSpace(rt))
		if rt != "" && rt != "INHERIT" {
			recordType = rt
		}
	}
	if recordType == "" {
		recordType = "A"
	}
	var content string
	switch recordType {
	case "A":
		content = strings.TrimSpace(ctx.IPv4)
	case "AAAA":
		content = strings.TrimSpace(ctx.IPv6)
	default:
		return factory.SyncResult{}, fmt.Errorf("unsupported record type: %s", recordType)
	}
	if content == "" {
		return factory.SyncResult{}, fmt.Errorf("no IP available for record type %s", recordType)
	}

	hostname := ""
	if v, ok := ctx.ProviderConfig["hostname"].(string); ok {
		hostname = strings.TrimSpace(v)
	}
	recordID := strings.TrimSpace(p.RecordID)
	if rid, ok := ctx.ProviderConfig["record_id"].(string); ok && strings.TrimSpace(rid) != "" {
		recordID = strings.TrimSpace(rid)
	}

	payload := map[string]any{
		"type":    recordType,
		"content": content,
		"ttl":     p.TTL,
		"proxied": p.Proxied,
		"comment": fmt.Sprintf("updated by Komari for %s", ctx.ClientUUID),
	}
	if hostname != "" {
		payload["name"] = hostname
	}
	if recordID == "" {
		if hostname == "" {
			return factory.SyncResult{}, fmt.Errorf("cloudflare record id is not configured and hostname is empty")
		}
		resolvedID, err := p.lookupRecordID(hostname, recordType)
		if err != nil {
			if !isRecordNotFound(err) {
				return factory.SyncResult{}, err
			}
			createdID, createErr := p.createRecord(payload)
			if createErr != nil {
				return factory.SyncResult{}, createErr
			}
			return factory.SyncResult{ResolvedRecordID: createdID}, nil
		}
		recordID = resolvedID
	}

	body, err := json.Marshal(payload)
	if err != nil {
		return factory.SyncResult{}, err
	}
	url := fmt.Sprintf("%s/zones/%s/dns_records/%s", cloudflareAPIBaseURL, p.ZoneID, recordID)
	req, err := http.NewRequest(http.MethodPatch, url, bytes.NewReader(body))
	if err != nil {
		return factory.SyncResult{}, err
	}
	p.applyHeaders(req)
	resp, err := (&http.Client{Timeout: 30 * time.Second}).Do(req)
	if err != nil {
		return factory.SyncResult{}, err
	}
	defer resp.Body.Close()
	respBody, _ := io.ReadAll(resp.Body)
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return factory.SyncResult{}, fmt.Errorf("cloudflare api failed with status %d: %s", resp.StatusCode, string(respBody))
	}
	var result struct {
		Success bool `json:"success"`
		Errors  []struct {
			Message string `json:"message"`
		} `json:"errors"`
	}
	if err := json.Unmarshal(respBody, &result); err != nil {
		return factory.SyncResult{}, err
	}
	if !result.Success {
		msgs := make([]string, 0, len(result.Errors))
		for _, e := range result.Errors {
			msgs = append(msgs, e.Message)
		}
		return factory.SyncResult{}, fmt.Errorf("cloudflare api returned error: %s", strings.Join(msgs, "; "))
	}
	return factory.SyncResult{ResolvedRecordID: recordID}, nil
}

func (p *Provider) lookupRecordID(hostname, recordType string) (string, error) {
	endpoint := fmt.Sprintf(
		"%s/zones/%s/dns_records?type=%s&name=%s",
		cloudflareAPIBaseURL,
		p.ZoneID,
		url.QueryEscape(recordType),
		url.QueryEscape(hostname),
	)
	req, err := http.NewRequest(http.MethodGet, endpoint, nil)
	if err != nil {
		return "", err
	}
	p.applyHeaders(req)
	resp, err := (&http.Client{Timeout: 30 * time.Second}).Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return "", fmt.Errorf("cloudflare lookup failed with status %d: %s", resp.StatusCode, string(body))
	}
	var result struct {
		Success bool `json:"success"`
		Errors  []struct {
			Message string `json:"message"`
		} `json:"errors"`
		Result []struct {
			ID string `json:"id"`
		} `json:"result"`
	}
	if err := json.Unmarshal(body, &result); err != nil {
		return "", err
	}
	if !result.Success {
		msgs := make([]string, 0, len(result.Errors))
		for _, e := range result.Errors {
			msgs = append(msgs, e.Message)
		}
		return "", fmt.Errorf("cloudflare lookup error: %s", strings.Join(msgs, "; "))
	}
	if len(result.Result) == 0 {
		return "", fmt.Errorf("cloudflare record not found for hostname=%s type=%s", hostname, recordType)
	}
	return result.Result[0].ID, nil
}

func (p *Provider) createRecord(payload map[string]any) (string, error) {
	body, err := json.Marshal(payload)
	if err != nil {
		return "", err
	}
	endpoint := fmt.Sprintf("%s/zones/%s/dns_records", cloudflareAPIBaseURL, p.ZoneID)
	req, err := http.NewRequest(http.MethodPost, endpoint, bytes.NewReader(body))
	if err != nil {
		return "", err
	}
	p.applyHeaders(req)
	resp, err := (&http.Client{Timeout: 30 * time.Second}).Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	respBody, _ := io.ReadAll(resp.Body)
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return "", fmt.Errorf("cloudflare create failed with status %d: %s", resp.StatusCode, string(respBody))
	}
	var result struct {
		Success bool `json:"success"`
		Errors  []struct {
			Message string `json:"message"`
		} `json:"errors"`
		Result struct {
			ID string `json:"id"`
		} `json:"result"`
	}
	if err := json.Unmarshal(respBody, &result); err != nil {
		return "", err
	}
	if !result.Success {
		msgs := make([]string, 0, len(result.Errors))
		for _, e := range result.Errors {
			msgs = append(msgs, e.Message)
		}
		return "", fmt.Errorf("cloudflare create error: %s", strings.Join(msgs, "; "))
	}
	if strings.TrimSpace(result.Result.ID) == "" {
		return "", fmt.Errorf("cloudflare create returned empty record id")
	}
	return strings.TrimSpace(result.Result.ID), nil
}

func isRecordNotFound(err error) bool {
	return err != nil && strings.Contains(err.Error(), "cloudflare record not found")
}

func (p *Provider) applyHeaders(req *http.Request) {
	req.Header.Set("Authorization", "Bearer "+p.APIToken)
	req.Header.Set("Content-Type", "application/json")
}

func init() {
	factory.RegisterDdnsProvider(func() factory.IDdnsProvider {
		return &Provider{}
	})
}
