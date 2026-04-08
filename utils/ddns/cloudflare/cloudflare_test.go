package cloudflare

import (
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/komari-monitor/komari/utils/ddns/factory"
)

func TestSyncCreatesRecordWhenLookupMisses(t *testing.T) {
	var lookupCalls int
	var createCalls int

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/zones/zone-1/dns_records":
			lookupCalls++
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `{"success":true,"result":[]}`)
		case r.Method == http.MethodPost && r.URL.Path == "/zones/zone-1/dns_records":
			createCalls++
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `{"success":true,"result":{"id":"created-record-id"}}`)
		default:
			t.Fatalf("unexpected request: %s %s", r.Method, r.URL.String())
		}
	}))
	defer server.Close()

	oldBaseURL := cloudflareAPIBaseURL
	cloudflareAPIBaseURL = server.URL
	defer func() { cloudflareAPIBaseURL = oldBaseURL }()

	provider := &Provider{Addition: Addition{
		APIToken:   "token",
		ZoneID:     "zone-1",
		RecordType: "A",
		TTL:        1,
	}}

	result, err := provider.Sync(factory.SyncContext{
		IPv4:       "1.2.3.4",
		ClientUUID: "client-1",
		ProviderConfig: map[string]any{
			"hostname": "node.example.com",
		},
	})
	if err != nil {
		t.Fatalf("Sync returned error: %v", err)
	}
	if result.ResolvedRecordID != "created-record-id" {
		t.Fatalf("unexpected resolved record id: %s", result.ResolvedRecordID)
	}
	if lookupCalls != 1 {
		t.Fatalf("expected 1 lookup call, got %d", lookupCalls)
	}
	if createCalls != 1 {
		t.Fatalf("expected 1 create call, got %d", createCalls)
	}
}

func TestSyncUpdatesExistingRecord(t *testing.T) {
	var lookupCalls int
	var patchCalls int

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/zones/zone-1/dns_records":
			lookupCalls++
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `{"success":true,"result":[{"id":"existing-record-id"}]}`)
		case r.Method == http.MethodPatch && r.URL.Path == "/zones/zone-1/dns_records/existing-record-id":
			patchCalls++
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `{"success":true,"errors":[]}`)
		default:
			t.Fatalf("unexpected request: %s %s", r.Method, r.URL.String())
		}
	}))
	defer server.Close()

	oldBaseURL := cloudflareAPIBaseURL
	cloudflareAPIBaseURL = server.URL
	defer func() { cloudflareAPIBaseURL = oldBaseURL }()

	provider := &Provider{Addition: Addition{
		APIToken:   "token",
		ZoneID:     "zone-1",
		RecordType: "A",
		TTL:        1,
	}}

	result, err := provider.Sync(factory.SyncContext{
		IPv4:       "1.2.3.4",
		ClientUUID: "client-1",
		ProviderConfig: map[string]any{
			"hostname": "node.example.com",
		},
	})
	if err != nil {
		t.Fatalf("Sync returned error: %v", err)
	}
	if result.ResolvedRecordID != "existing-record-id" {
		t.Fatalf("unexpected resolved record id: %s", result.ResolvedRecordID)
	}
	if lookupCalls != 1 {
		t.Fatalf("expected 1 lookup call, got %d", lookupCalls)
	}
	if patchCalls != 1 {
		t.Fatalf("expected 1 patch call, got %d", patchCalls)
	}
}

func TestSyncUsesProvidedRecordIDWithoutLookup(t *testing.T) {
	var lookupCalls int
	var patchCalls int

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case r.Method == http.MethodPatch && r.URL.Path == "/zones/zone-1/dns_records/direct-record-id":
			patchCalls++
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `{"success":true,"errors":[]}`)
		case r.Method == http.MethodGet:
			lookupCalls++
			w.WriteHeader(http.StatusInternalServerError)
			_, _ = io.WriteString(w, `{"success":false}`)
		default:
			t.Fatalf("unexpected request: %s %s", r.Method, r.URL.String())
		}
	}))
	defer server.Close()

	oldBaseURL := cloudflareAPIBaseURL
	cloudflareAPIBaseURL = server.URL
	defer func() { cloudflareAPIBaseURL = oldBaseURL }()

	provider := &Provider{Addition: Addition{
		APIToken:   "token",
		ZoneID:     "zone-1",
		RecordType: "A",
		TTL:        1,
	}}

	result, err := provider.Sync(factory.SyncContext{
		IPv4:       "1.2.3.4",
		ClientUUID: "client-1",
		ProviderConfig: map[string]any{
			"hostname":  "node.example.com",
			"record_id": "direct-record-id",
		},
	})
	if err != nil {
		t.Fatalf("Sync returned error: %v", err)
	}
	if result.ResolvedRecordID != "direct-record-id" {
		t.Fatalf("unexpected resolved record id: %s", result.ResolvedRecordID)
	}
	if lookupCalls != 0 {
		t.Fatalf("expected 0 lookup call, got %d", lookupCalls)
	}
	if patchCalls != 1 {
		t.Fatalf("expected 1 patch call, got %d", patchCalls)
	}
}

func TestCloudflareRecordNotFoundDetection(t *testing.T) {
	err := fmt.Errorf("cloudflare record not found for hostname=node.example.com type=A")
	if !isRecordNotFound(err) {
		t.Fatal("expected not found error to be detected")
	}
	if isRecordNotFound(fmt.Errorf("other error")) {
		t.Fatal("expected other error to not be detected")
	}
}
