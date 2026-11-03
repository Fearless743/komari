mod support;

use std::sync::Arc;

use komari_ddns::DdnsProvider as _;
use komari_ddns::error::DdnsError;
use komari_ddns::factory::DdnsFactory;
use komari_ddns::provider::{ProviderConfig, SyncContext};
use komari_ddns::providers::cloudflare::CloudflareProvider;
use komari_ddns::providers::empty::EmptyProvider;
use komari_ddns::providers::webhook::{WebhookProvider, extract_json_path, replace_placeholders};
use serde_json::{Value, json};
use support::{MockResponse, MockServer};

fn context(ipv4: &str, ipv6: &str) -> SyncContext {
    SyncContext {
        ipv4: ipv4.to_owned(),
        ipv6: ipv6.to_owned(),
        client_uuid: "client-1".to_owned(),
        client_name: "node-1".to_owned(),
        triggered_by: "test".to_owned(),
        ..SyncContext::default()
    }
}

fn config(pairs: &[(&str, &str)]) -> ProviderConfig {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), Value::String((*value).to_owned())))
        .collect()
}

fn cloudflare(server: &MockServer, settings: Value) -> CloudflareProvider {
    let mut provider = CloudflareProvider::with_base_url(server.url());
    provider
        .set_configuration(settings)
        .expect("cloudflare config");
    provider
}

#[tokio::test]
async fn cloudflare_creates_record_when_lookup_misses() {
    let server =
        MockServer::start(
            |request| match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/zones/zone-1/dns_records") => {
                    MockResponse::json(200, r#"{"success":true,"result":[]}"#)
                }
                ("POST", "/zones/zone-1/dns_records") => MockResponse::json(
                    200,
                    r#"{"success":true,"result":{"id":"created-record-id"}}"#,
                ),
                _ => MockResponse::json(500, r#"{"success":false}"#),
            },
        )
        .await;

    let provider = cloudflare(
        &server,
        json!({"api_token":"token","zone_id":"zone-1","record_type":"A","ttl":300,"proxied":false}),
    );
    let mut ctx = context("1.2.3.4", "");
    ctx.provider_config = config(&[("hostname", "node.example.com")]);

    let result = provider.sync(&ctx).await.expect("cloudflare sync");
    assert_eq!(result.resolved_record_id, "created-record-id");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert!(requests[0].query.contains("type=A"));
    assert!(requests[0].query.contains("name=node.example.com"));
    assert!(requests[0].query.contains("per_page=10"));
    assert_eq!(requests[0].header("authorization"), Some("Bearer token"));

    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].header("content-type"), Some("application/json"));
    let body = requests[1].json();
    assert_eq!(body["type"], "A");
    assert_eq!(body["content"], "1.2.3.4");
    assert_eq!(body["name"], "node.example.com");
    assert_eq!(body["ttl"], 300);
    assert_eq!(body["proxied"], false);
    assert_eq!(body["comment"], "updated by Komari for client-1");
}

#[tokio::test]
async fn cloudflare_updates_existing_record() {
    let server = MockServer::start(|request| match (request.method.as_str(), request.path.as_str())
    {
        ("GET", "/zones/zone-1/dns_records") => MockResponse::json(
            200,
            r#"{"success":true,"result":[{"id":"existing-record-id","comment":"updated by Komari for old"}]}"#,
        ),
        ("PATCH", "/zones/zone-1/dns_records/existing-record-id") => {
            MockResponse::json(200, r#"{"success":true,"errors":[]}"#)
        }
        _ => MockResponse::json(500, r#"{"success":false}"#),
    })
    .await;

    let provider = cloudflare(
        &server,
        json!({"api_token":"token","zone_id":"zone-1","record_type":"A","ttl":1}),
    );
    let mut ctx = context("1.2.3.4", "");
    ctx.provider_config = config(&[("hostname", "node.example.com")]);

    let result = provider.sync(&ctx).await.expect("cloudflare sync");
    assert_eq!(result.resolved_record_id, "existing-record-id");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, "PATCH");
    assert_eq!(
        requests[1].path,
        "/zones/zone-1/dns_records/existing-record-id"
    );
    assert_eq!(requests[1].json()["content"], "1.2.3.4");
}

#[tokio::test]
async fn cloudflare_uses_provided_record_id_without_lookup() {
    let server =
        MockServer::start(
            |request| match (request.method.as_str(), request.path.as_str()) {
                ("PATCH", "/zones/zone-1/dns_records/direct-record-id") => {
                    MockResponse::json(200, r#"{"success":true,"errors":[]}"#)
                }
                _ => MockResponse::json(500, r#"{"success":false}"#),
            },
        )
        .await;

    let provider = cloudflare(
        &server,
        json!({"api_token":"token","zone_id":"zone-1","record_type":"A"}),
    );
    let mut ctx = context("1.2.3.4", "");
    ctx.provider_config = config(&[
        ("hostname", "node.example.com"),
        ("record_id", "direct-record-id"),
    ]);

    let result = provider.sync(&ctx).await.expect("cloudflare sync");
    assert_eq!(result.resolved_record_id, "direct-record-id");
    assert_eq!(server.request_count(), 1);
    assert_eq!(server.requests()[0].method, "PATCH");
}

#[tokio::test]
async fn cloudflare_honours_inherit_and_client_record_type() {
    let server = MockServer::start(|request| {
        if request.method == "PATCH" {
            MockResponse::json(200, r#"{"success":true,"errors":[]}"#)
        } else {
            MockResponse::json(500, r#"{"success":false}"#)
        }
    })
    .await;

    let provider = cloudflare(
        &server,
        json!({"api_token":"token","zone_id":"zone-1","record_type":"A"}),
    );

    let mut inherited = context("1.2.3.4", "2001:db8::1");
    inherited.provider_config = config(&[
        ("hostname", "node.example.com"),
        ("record_id", "record-1"),
        ("record_type", "inherit"),
    ]);
    provider.sync(&inherited).await.expect("inherited sync");

    let mut overridden = context("1.2.3.4", "2001:db8::1");
    overridden.provider_config = config(&[
        ("hostname", "node.example.com"),
        ("record_id", "record-1"),
        ("record_type", "aaaa"),
    ]);
    provider.sync(&overridden).await.expect("overridden sync");

    let bodies: Vec<Value> = server
        .requests()
        .iter()
        .map(|request| request.json())
        .collect();
    assert_eq!(bodies[0]["type"], "A");
    assert_eq!(bodies[0]["content"], "1.2.3.4");
    assert_eq!(bodies[1]["type"], "AAAA");
    assert_eq!(bodies[1]["content"], "2001:db8::1");
}

#[tokio::test]
async fn cloudflare_rejects_unsupported_type_and_missing_ip() {
    let server = MockServer::start(|_| MockResponse::json(200, r#"{"success":true}"#)).await;
    let provider = cloudflare(
        &server,
        json!({"api_token":"token","zone_id":"zone-1","record_type":"MX"}),
    );
    let mut ctx = context("1.2.3.4", "");
    ctx.provider_config = config(&[("record_id", "record-1")]);
    let error = provider.sync(&ctx).await.expect_err("unsupported type");
    assert!(error.to_string().contains("unsupported record type: MX"));

    let mut ipv6 = context("1.2.3.4", "");
    ipv6.provider_config = config(&[("record_id", "record-1"), ("record_type", "AAAA")]);
    let error = provider.sync(&ipv6).await.expect_err("missing ipv6");
    assert!(
        error
            .to_string()
            .contains("no IP available for record type AAAA")
    );
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn cloudflare_surfaces_rate_limits_and_api_errors() {
    let server = MockServer::start(|_| {
        MockResponse::json(
            429,
            r#"{"success":false,"errors":[{"message":"rate limited"}]}"#,
        )
    })
    .await;
    let provider = cloudflare(
        &server,
        json!({"api_token":"token","zone_id":"zone-1","record_type":"A"}),
    );
    let mut ctx = context("1.2.3.4", "");
    ctx.provider_config = config(&[("record_id", "record-1")]);
    let error = provider.sync(&ctx).await.expect_err("rate limited");
    assert!(matches!(error, DdnsError::HttpStatus { status: 429, .. }));
    assert!(error.to_string().contains("429"));

    let server = MockServer::start(|_| {
        MockResponse::json(
            200,
            r#"{"success":false,"errors":[{"message":"invalid token"}]}"#,
        )
    })
    .await;
    let provider = cloudflare(
        &server,
        json!({"api_token":"token","zone_id":"zone-1","record_type":"A"}),
    );
    let error = provider
        .sync(&ctx)
        .await
        .expect_err("application-level error");
    assert!(error.to_string().contains("invalid token"));
}

#[tokio::test]
async fn cloudflare_requires_token_and_zone() {
    let server = MockServer::start(|_| MockResponse::json(200, r#"{"success":true}"#)).await;
    let provider = cloudflare(&server, json!({"record_type":"A"}));
    let error = provider
        .sync(&context("1.2.3.4", ""))
        .await
        .expect_err("missing credentials");
    assert!(error.to_string().contains("not fully configured"));
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn cloudflare_lookup_error_does_not_create_record() {
    let server = MockServer::start(|_| MockResponse::json(500, r#"{"success":false}"#)).await;
    let provider = cloudflare(
        &server,
        json!({"api_token":"token","zone_id":"zone-1","record_type":"A"}),
    );
    let mut ctx = context("1.2.3.4", "");
    ctx.provider_config = config(&[("hostname", "node.example.com")]);
    let error = provider.sync(&ctx).await.expect_err("lookup failure");
    assert!(matches!(error, DdnsError::HttpStatus { status: 500, .. }));
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn cloudflare_create_failure_is_reported() {
    let server = MockServer::start(|request| {
        if request.method == "GET" {
            MockResponse::json(200, r#"{"success":true,"result":[]}"#)
        } else {
            MockResponse::json(
                500,
                r#"{"success":false,"errors":[{"message":"create failed"}]}"#,
            )
        }
    })
    .await;
    let provider = cloudflare(
        &server,
        json!({"api_token":"token","zone_id":"zone-1","record_type":"A"}),
    );
    let mut ctx = context("1.2.3.4", "");
    ctx.provider_config = config(&[("hostname", "node.example.com")]);
    let error = provider.sync(&ctx).await.expect_err("create failure");
    assert!(matches!(error, DdnsError::HttpStatus { status: 500, .. }));
}

#[tokio::test]
async fn webhook_sends_templated_request_and_extracts_record_id() {
    let server = MockServer::start(|_| {
        MockResponse::json(201, r#"{"data":{"record_id":"webhook-record-1"}}"#)
    })
    .await;

    let mut provider = WebhookProvider::new();
    provider
        .set_configuration(json!({
            "url": format!("{}/hook", server.url()),
            "method": "PUT",
            "content_type": "application/json",
            "headers": "{\"X-Token\":\"secret\"}",
            "body": "{\"uuid\":\"{{client_uuid}}\",\"name\":\"{{client_name}}\",\"ipv4\":\"{{ipv4}}\",\"ipv6\":\"{{ipv6}}\",\"by\":\"{{triggered_by}}\",\"host\":\"{{hostname}}\",\"type\":\"{{record_type}}\"}",
            "response_json_path": "data.record_id"
        }))
        .expect("webhook config");

    let mut ctx = context("1.2.3.4", "2001:db8::1");
    ctx.provider_config = config(&[("hostname", "node.example.com"), ("record_type", "A")]);
    let result = provider.sync(&ctx).await.expect("webhook sync");
    assert_eq!(result.resolved_record_id, "webhook-record-1");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].path, "/hook");
    assert_eq!(requests[0].header("x-token"), Some("secret"));
    assert_eq!(requests[0].header("content-type"), Some("application/json"));
    let body = requests[0].json();
    assert_eq!(body["uuid"], "client-1");
    assert_eq!(body["name"], "node-1");
    assert_eq!(body["ipv4"], "1.2.3.4");
    assert_eq!(body["ipv6"], "2001:db8::1");
    assert_eq!(body["by"], "test");
    assert_eq!(body["host"], "node.example.com");
    assert_eq!(body["type"], "A");
}

#[tokio::test]
async fn webhook_reports_status_errors_and_invalid_configuration() {
    let server = MockServer::start(|_| MockResponse::text(500, "boom")).await;
    let mut provider = WebhookProvider::new();
    provider
        .set_configuration(json!({"url": format!("{}/hook", server.url()), "body": "{}"}))
        .expect("webhook config");
    let error = provider
        .sync(&context("1.2.3.4", ""))
        .await
        .expect_err("webhook 500");
    assert!(matches!(error, DdnsError::HttpStatus { status: 500, .. }));
    assert!(error.to_string().contains("boom"));

    let mut invalid = WebhookProvider::new();
    invalid
        .set_configuration(json!({"url": format!("{}/hook", server.url()), "headers": "not-json"}))
        .expect("webhook config");
    let error = invalid
        .sync(&context("1.2.3.4", ""))
        .await
        .expect_err("invalid headers");
    assert!(error.to_string().contains("invalid headers json"));

    let mut missing = WebhookProvider::new();
    missing
        .set_configuration(json!({}))
        .expect("empty webhook config");
    let error = missing
        .sync(&context("1.2.3.4", ""))
        .await
        .expect_err("missing url");
    assert!(error.to_string().contains("webhook URL is not configured"));
}

#[tokio::test]
async fn cloudflare_deletes_record_with_authentication() {
    let server = MockServer::start(|request| {
        if request.method == "DELETE" && request.path == "/zones/zone-1/dns_records/record-1" {
            MockResponse::json(200, r#"{"success":true,"errors":[]}"#)
        } else {
            MockResponse::json(500, r#"{"success":false}"#)
        }
    })
    .await;
    let provider = cloudflare(
        &server,
        json!({"api_token":"token","zone_id":"zone-1","record_type":"A"}),
    );

    provider.delete_record("record-1").await.expect("delete");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(requests[0].header("authorization"), Some("Bearer token"));

    let empty = EmptyProvider;
    empty.delete_record("record-1").await.expect("empty delete");

    let mut webhook = WebhookProvider::new();
    webhook.set_configuration(json!({})).expect("config");
    let error = webhook
        .delete_record("record-1")
        .await
        .expect_err("webhook deletion is unsupported");
    assert!(error.to_string().contains("does not support deleting"));
}

#[test]
fn webhook_helpers_match_go() {
    let ctx = context("1.2.3.4", "2001:db8::1");
    let rendered = replace_placeholders("{{ipv4}}|{{ipv6}}|{{client_uuid}}", &ctx);
    assert_eq!(rendered, "1.2.3.4|2001:db8::1|client-1");

    assert_eq!(
        extract_json_path(r#"{"record_id":"abc"}"#, "record_id"),
        "abc"
    );
    assert_eq!(
        extract_json_path(r#"{"data":{"items":[{"id":"nested"}]}}"#, "data.items.0.id"),
        "nested"
    );
    assert_eq!(extract_json_path(r#"{"data":{}}"#, "data.id"), "");
    assert_eq!(extract_json_path("not json", "id"), "");
    assert_eq!(extract_json_path(r#"{"id":123}"#, "id"), "");
}

#[tokio::test]
async fn empty_provider_is_a_noop() {
    let mut provider = EmptyProvider;
    provider
        .set_configuration(json!({}))
        .expect("empty configuration");
    let result = provider
        .sync(&context("1.2.3.4", ""))
        .await
        .expect("empty sync");
    assert!(result.resolved_record_id.is_empty());
    assert_eq!(provider.name(), "empty");
}

#[test]
fn factory_exposes_builtins_and_rejects_unknown_providers() {
    let factory = DdnsFactory::with_builtins();
    assert_eq!(
        factory.provider_names(),
        vec!["cloudflare", "empty", "webhook"]
    );

    let configs = factory.get_provider_configs();
    let cloudflare = &configs["cloudflare"];
    assert_eq!(cloudflare.len(), 6);
    assert!(
        cloudflare
            .iter()
            .any(|item| item.name == "api_token" && item.required)
    );
    assert!(cloudflare.iter().any(|item| item.name == "record_type"
        && item.item_type == "option"
        && item.options == "A,AAAA"));

    assert!(factory.get_constructor("cloudflare").is_ok());
    assert!(matches!(
        factory.get_constructor("missing"),
        Err(DdnsError::ProviderNotFound(name)) if name == "missing"
    ));
    assert!(factory.create("cloudflare").is_ok());

    let empty = DdnsFactory::new();
    assert!(empty.provider_names().is_empty());
    assert!(matches!(
        empty.create("cloudflare"),
        Err(DdnsError::ProviderNotFound(_))
    ));
}

#[test]
fn provider_configuration_round_trips() {
    let mut provider = CloudflareProvider::new();
    provider
        .set_configuration(json!({
            "api_token": "token",
            "zone_id": "zone",
            "record_type": "AAAA",
            "ttl": 120,
            "proxied": true
        }))
        .expect("valid configuration");
    let configuration = provider.configuration();
    assert_eq!(configuration["api_token"], "token");
    assert_eq!(configuration["zone_id"], "zone");
    assert_eq!(configuration["record_type"], "AAAA");
    assert_eq!(configuration["ttl"], 120);
    assert_eq!(configuration["proxied"], true);

    assert!(
        provider
            .set_configuration(json!({"ttl": "not-a-number"}))
            .is_err()
    );
}

#[test]
fn unknown_configuration_fields_are_ignored_like_go() {
    let factory = DdnsFactory::with_builtins();
    let mut provider = factory.create("cloudflare").expect("provider");
    provider
        .set_configuration(json!({"api_token":"token","zone_id":"zone","extra":"ignored"}))
        .expect("unknown fields do not fail");
    assert_eq!(provider.configuration()["api_token"], "token");
}

#[test]
fn constructor_arcs_are_shareable() {
    let factory = DdnsFactory::with_builtins();
    let constructor = factory.get_constructor("empty").expect("constructor");
    let _shared: Arc<_> = constructor;
}
