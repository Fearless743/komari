use komari_db::Db;
use komari_db::models::DdnsProvider as ProviderModel;
use komari_ddns::factory::DdnsFactory;
use komari_ddns::providers::cloudflare::CloudflareProvider;
use komari_ddns::providers::webhook::WebhookProvider;
use komari_ddns::{
    DdnsProvider, redact_addition, redact_addition_with_fields, redact_provider_configuration,
};
use serde_json::{Value, json};

const CLOUDFLARE_ADDITION: &str = r#"{"api_token":"cloudflare-token-secret","zone_id":"cloudflare-zone-secret","record_id":"record-1","record_type":"A","ttl":120,"proxied":true}"#;

#[test]
fn cloudflare_redacts_token_and_zone_but_keeps_other_fields() {
    let redacted = redact_addition("cloudflare", CLOUDFLARE_ADDITION);
    assert_eq!(
        redacted,
        json!({
            "record_id": "record-1",
            "record_type": "A",
            "ttl": 120,
            "proxied": true
        })
    );
    let serialized = redacted.to_string();
    assert!(!serialized.contains("cloudflare-token-secret"));
    assert!(!serialized.contains("cloudflare-zone-secret"));
}

#[test]
fn webhook_redacts_url_headers_and_body_but_keeps_public_settings() {
    let addition = r#"{"url":"https://hooks.example.com/abc","method":"POST","content_type":"application/json","headers":"{\"Authorization\":\"Bearer header-secret\"}","body":"{\"token\":\"body-secret\"}","response_json_path":"data.record_id"}"#;
    let redacted = redact_addition("webhook", addition);
    assert_eq!(
        redacted,
        json!({
            "method": "POST",
            "content_type": "application/json",
            "response_json_path": "data.record_id"
        })
    );
    let serialized = redacted.to_string();
    assert!(!serialized.contains("hooks.example.com"));
    assert!(!serialized.contains("header-secret"));
    assert!(!serialized.contains("body-secret"));
}

#[test]
fn webhook_removes_url_credentials_and_path_or_query_tokens() {
    for addition in [
        r#"{"url":"https://user:password@hooks.example.com/secret-path","method":"POST","headers":"{\"Authorization\":\"Bearer header-secret\"}","body":"body-secret"}"#,
        r#"{"url":"https://hooks.slack.com/services/T000/B000/path-secret?token=query-secret","method":"POST"}"#,
        r#"{"url":"https://discord.com/api/webhooks/123/path-secret","method":"POST"}"#,
    ] {
        let redacted = redact_addition("webhook", addition);
        assert_eq!(redacted, json!({"method": "POST"}));
        let serialized = redacted.to_string();
        assert!(!serialized.contains("user:password"));
        assert!(!serialized.contains("path-secret"));
        assert!(!serialized.contains("query-secret"));
        assert!(!serialized.contains("header-secret"));
        assert!(!serialized.contains("body-secret"));
    }
}

#[test]
fn empty_provider_addition_is_unchanged() {
    assert_eq!(redact_addition("empty", "{}"), json!({}));
    assert_eq!(
        redact_addition("empty", r#"{"arbitrary":"value"}"#),
        json!({"arbitrary": "value"})
    );
}

#[test]
fn invalid_json_and_non_objects_are_safe() {
    for addition in ["not json", "null", "[]", r#""secret""#, "42", ""] {
        let redacted = redact_addition("cloudflare", addition);
        assert_eq!(redacted, Value::Object(Default::default()));
        assert!(!redacted.to_string().contains("secret"));
    }
}

#[test]
fn unknown_providers_return_an_empty_object() {
    assert_eq!(
        redact_addition("missing", r#"{"api_token":"secret"}"#),
        json!({})
    );
}

#[test]
fn explicit_sensitive_field_list_is_honoured() {
    let redacted = redact_addition_with_fields(
        "custom",
        r#"{"secret":"value","keep":"visible"}"#,
        &["secret"],
    );
    assert_eq!(redacted, json!({"keep": "visible"}));

    let redacted = redact_addition_with_fields(
        "webhook",
        r#"{"url":"https://hooks.example.com/path-secret","headers":"header-secret","method":"POST"}"#,
        &["headers"],
    );
    assert_eq!(redacted, json!({"method": "POST"}));
}

#[test]
fn database_addition_stays_complete_and_internal_use_keeps_secrets() {
    let db = Db::open_in_memory().expect("database");
    db.migrate().expect("migrate");
    db.ddns_providers()
        .save(&ProviderModel::new("cloudflare", CLOUDFLARE_ADDITION))
        .expect("save provider");

    let stored = db
        .ddns_providers()
        .get("cloudflare")
        .expect("read provider")
        .expect("provider row");
    let _ = redact_addition("cloudflare", &stored.addition);

    let reloaded = db
        .ddns_providers()
        .get("cloudflare")
        .expect("read provider")
        .expect("provider row");
    assert_eq!(reloaded.addition, CLOUDFLARE_ADDITION);

    let mut provider = CloudflareProvider::new();
    provider
        .set_configuration(
            serde_json::from_str(&reloaded.addition).expect("provider configuration"),
        )
        .expect("internal provider still reads full config");
    assert_eq!(provider.config().api_token, "cloudflare-token-secret");
    assert_eq!(provider.config().zone_id, "cloudflare-zone-secret");
}

#[test]
fn debug_output_does_not_contain_secrets() {
    let mut cloudflare = CloudflareProvider::new();
    cloudflare
        .set_configuration(json!({
            "api_token": "debug-token-secret",
            "zone_id": "debug-zone-secret",
            "record_id": "record-1"
        }))
        .expect("cloudflare config");
    let debug = format!("{:?}", cloudflare.config());
    assert!(!debug.contains("debug-token-secret"));
    assert!(!debug.contains("debug-zone-secret"));
    assert!(debug.contains("<redacted>"));

    let mut webhook = WebhookProvider::new();
    webhook
        .set_configuration(json!({
            "url": "https://user:debug-password@example.com/hook",
            "headers": "{\"Authorization\":\"Bearer debug-header-secret\"}",
            "body": "debug-body-secret"
        }))
        .expect("webhook config");
    let debug = format!("{:?}", webhook.config());
    assert!(!debug.contains("debug-password"));
    assert!(!debug.contains("debug-header-secret"));
    assert!(!debug.contains("debug-body-secret"));

    let redacted = redact_provider_configuration(&webhook);
    assert!(!redacted.to_string().contains("debug-header-secret"));
}

#[test]
fn configuration_errors_do_not_echo_secret_values() {
    let error = CloudflareProvider::new()
        .set_configuration(json!("cloudflare-secret-token"))
        .expect_err("top-level string is invalid");
    assert!(!error.to_string().contains("cloudflare-secret-token"));
    assert!(
        error
            .to_string()
            .contains("invalid cloudflare configuration")
    );

    let error = WebhookProvider::new()
        .set_configuration(json!("webhook-secret-token"))
        .expect_err("top-level string is invalid");
    assert!(!error.to_string().contains("webhook-secret-token"));
    assert!(error.to_string().contains("invalid webhook configuration"));
}

#[test]
fn sync_context_debug_redacts_provider_configuration() {
    let mut provider_config = serde_json::Map::new();
    provider_config.insert("api_token".to_owned(), json!("context-secret-token"));
    provider_config.insert("hostname".to_owned(), json!("node.example.com"));
    let context = komari_ddns::SyncContext {
        ipv4: "1.2.3.4".to_owned(),
        client_uuid: "client-1".to_owned(),
        provider_config,
        ..komari_ddns::SyncContext::default()
    };
    let debug = format!("{context:?}");
    assert!(!debug.contains("context-secret-token"));
    assert!(!debug.contains("node.example.com"));
    assert!(debug.contains("<redacted>"));
}

#[test]
fn factory_declares_sensitive_fields() {
    let factory = DdnsFactory::with_builtins();
    assert_eq!(
        factory.sensitive_fields("cloudflare"),
        Some(vec!["api_token", "zone_id"])
    );
    assert_eq!(
        factory.sensitive_fields("webhook"),
        Some(vec!["url", "headers", "body"])
    );
    assert_eq!(factory.sensitive_fields("empty"), Some(Vec::new()));
    assert_eq!(factory.sensitive_fields("missing"), None);
}
