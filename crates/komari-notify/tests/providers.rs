//! Provider request-construction and configuration tests.

mod support;

use std::sync::Arc;

use komari_db::Db;
use komari_notify::providers::bark::{BarkConfig, BarkSender};
use komari_notify::providers::email::{EmailConfig, EmailSender};
use komari_notify::providers::empty::EmptySender;
use komari_notify::providers::serverchan3::{ServerChan3Config, ServerChan3Sender};
use komari_notify::providers::serverchanturbo::{ServerChanTurboConfig, ServerChanTurboSender};
use komari_notify::providers::telegram::{TelegramConfig, TelegramSender};
use komari_notify::providers::webhook::{DEFAULT_BODY, WebhookConfig, WebhookSender};
use komari_notify::{
    HttpResponse, MessageSender, NotifyError, get_provider_configuration, initialize_defaults,
    load_provider, registry,
};
use support::{MockEmail, MockHttp};

#[tokio::test]
async fn telegram_builds_go_compatible_form_request() {
    let transport = Arc::new(MockHttp::with_json(&serde_json::json!({"ok": true})));
    let sender = TelegramSender::with_transport(
        TelegramConfig {
            bot_token: "token".to_owned(),
            chat_id: "42".to_owned(),
            message_thread_id: "7".to_owned(),
            endpoint: "https://tg.example/bot".to_owned(),
        },
        transport.clone(),
    );

    sender.send_text("hello & bye", "Title").await.unwrap();

    let request = transport.last_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, "https://tg.example/bottoken/sendMessage");
    assert_eq!(
        request.header_value("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(
        request.body_text().unwrap(),
        "chat_id=42&message_thread_id=7&parse_mode=HTML&text=%3Cb%3ETitle%3C%2Fb%3E%0Ahello+%26+bye"
    );
}

#[tokio::test]
async fn telegram_rejects_empty_message_and_api_error() {
    let transport = Arc::new(MockHttp::with_json(&serde_json::json!({
        "ok": false,
        "description": "chat not found"
    })));
    let sender = TelegramSender::with_transport(TelegramConfig::default(), transport);
    assert!(matches!(
        sender.send_text("", "").await,
        Err(NotifyError::EmptyMessage)
    ));

    let transport = Arc::new(MockHttp::with_json(&serde_json::json!({
        "ok": false,
        "description": "chat not found"
    })));
    let sender = TelegramSender::with_transport(
        TelegramConfig {
            bot_token: "t".to_owned(),
            chat_id: "1".to_owned(),
            ..TelegramConfig::default()
        },
        transport,
    );
    let error = sender.send_text("hello", "").await.unwrap_err();
    assert!(error.to_string().contains("chat not found"));
}

#[tokio::test]
async fn bark_posts_expected_payload_and_url() {
    let transport = Arc::new(MockHttp::with_json(&serde_json::json!({
        "code": 200,
        "message": "ok"
    })));
    let sender = BarkSender::with_transport(
        BarkConfig {
            server_url: "https://bark.example/".to_owned(),
            device_key: "device".to_owned(),
            icon: "icon.png".to_owned(),
            level: "critical".to_owned(),
        },
        transport.clone(),
    );

    sender.send_text("body", "title").await.unwrap();
    let request = transport.last_request();
    assert_eq!(request.url, "https://bark.example/push");
    let payload: serde_json::Value =
        serde_json::from_slice(request.body.as_deref().unwrap()).unwrap();
    assert_eq!(payload["body"], "body");
    assert_eq!(payload["device_key"], "device");
    assert_eq!(payload["title"], "title");
    assert_eq!(payload["icon"], "icon.png");
    assert_eq!(payload["level"], "critical");
}

#[tokio::test]
async fn bark_requires_device_key_and_treats_non_json_as_success() {
    let transport = Arc::new(MockHttp::new(vec![HttpResponse::new(200, b"ok".to_vec())]));
    let missing = BarkSender::with_transport(BarkConfig::default(), transport.clone());
    assert!(missing.send_text("body", "").await.is_err());

    let sender = BarkSender::with_transport(
        BarkConfig {
            device_key: "device".to_owned(),
            ..BarkConfig::default()
        },
        transport,
    );
    sender.send_text("body", "").await.unwrap();
}

#[tokio::test]
async fn webhook_post_escapes_json_and_sets_headers_and_basic_auth() {
    let transport = Arc::new(MockHttp::new(vec![HttpResponse::new(204, Vec::new())]));
    let sender = WebhookSender::with_transport(
        WebhookConfig {
            url: "https://hook.example/path".to_owned(),
            method: "post".to_owned(),
            headers: r#"{"X-Test":"yes"}"#.to_owned(),
            username: "user".to_owned(),
            password: "pass".to_owned(),
            ..WebhookConfig::default()
        },
        transport.clone(),
    );

    sender.send_text("line\n\"quoted\"", "title").await.unwrap();
    let request = transport.last_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.header_value("X-Test"), Some("yes"));
    assert_eq!(
        request.header_value("Authorization"),
        Some("Basic dXNlcjpwYXNz")
    );
    let body = request.body_text().unwrap();
    assert!(body.contains(r#""message":"line\u000A\u0022quoted\u0022""#));
    assert!(body.contains(r#""title":"title""#));
}

#[tokio::test]
async fn webhook_get_substitutes_raw_values_into_url() {
    let transport = Arc::new(MockHttp::new(vec![HttpResponse::new(200, Vec::new())]));
    let sender = WebhookSender::with_transport(
        WebhookConfig {
            url: "https://hook.example/{{title}}?m={{message}}".to_owned(),
            method: "GET".to_owned(),
            ..WebhookConfig::default()
        },
        transport.clone(),
    );

    sender.send_text("hello world", "alert").await.unwrap();
    assert_eq!(
        transport.last_request().url,
        "https://hook.example/alert?m=hello world"
    );
}

#[tokio::test]
async fn webhook_rejects_unknown_method_and_non_2xx() {
    let transport = Arc::new(MockHttp::new(vec![HttpResponse::new(
        500,
        b"boom".to_vec(),
    )]));
    let sender = WebhookSender::with_transport(
        WebhookConfig {
            url: "https://hook.example".to_owned(),
            method: "PUT".to_owned(),
            ..WebhookConfig::default()
        },
        transport.clone(),
    );
    assert!(sender.send_text("x", "y").await.is_err());

    let sender = WebhookSender::with_transport(
        WebhookConfig {
            url: "https://hook.example".to_owned(),
            method: "POST".to_owned(),
            ..WebhookConfig::default()
        },
        transport,
    );
    let error = sender.send_text("x", "y").await.unwrap_err();
    assert!(error.to_string().contains("500"));
    assert!(error.to_string().contains("boom"));
}

#[tokio::test]
async fn email_builds_mime_message_and_delivers_envelope() {
    let transport = Arc::new(MockEmail::new());
    let sender = EmailSender::with_transport(
        EmailConfig {
            host: "smtp.example".to_owned(),
            port: 587,
            username: "user".to_owned(),
            password: "pass".to_owned(),
            sender: "Komari <bot@example.com>".to_owned(),
            receiver: "a@example.com, B <b@example.com>".to_owned(),
            ..EmailConfig::default()
        },
        transport.clone(),
    );

    sender.send_text("hello\nworld", "状态提醒").await.unwrap();
    let envelopes = transport.envelopes();
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0].from, "bot@example.com");
    assert_eq!(
        envelopes[0].recipients,
        vec!["a@example.com".to_owned(), "b@example.com".to_owned()]
    );
    let raw = envelopes[0].raw_text();
    assert!(raw.contains("Subject: "));
    assert!(raw.contains("MIME-Version: 1.0"));
    assert!(raw.contains("text/plain; charset=utf-8"));
    assert!(raw.contains("quoted-printable") || raw.contains("hello"));
}

#[tokio::test]
async fn email_requires_every_field_go_checks() {
    let transport = Arc::new(MockEmail::new());
    let sender = EmailSender::with_transport(EmailConfig::default(), transport);
    let error = sender.send_text("hello", "title").await.unwrap_err();
    assert!(error.to_string().contains("not fully configured"));
}

#[tokio::test]
async fn serverchan3_builds_title_desp_and_tags() {
    let transport = Arc::new(MockHttp::new(vec![HttpResponse::new(200, Vec::new())]));
    let sender = ServerChan3Sender::with_transport(
        ServerChan3Config {
            api_url: "https://sc3.example/send/key.send".to_owned(),
            tags: " a | b || ".to_owned(),
        },
        transport.clone(),
    );

    sender.send_text(" body ", " title ").await.unwrap();
    let request = transport.last_request();
    assert_eq!(request.url, "https://sc3.example/send/key.send");
    let payload: serde_json::Value =
        serde_json::from_slice(request.body.as_deref().unwrap()).unwrap();
    assert_eq!(payload["title"], "title");
    assert_eq!(payload["desp"], "body");
    assert_eq!(payload["tags"], "a|b");
}

#[tokio::test]
async fn serverchanturbo_adds_only_configured_optional_fields() {
    let transport = Arc::new(MockHttp::new(vec![HttpResponse::new(200, Vec::new())]));
    let sender = ServerChanTurboSender::with_transport(
        ServerChanTurboConfig {
            api_url: "https://sct.example/key.send".to_owned(),
            channel: "9|66".to_owned(),
            noip: "1".to_owned(),
            openid: String::new(),
        },
        transport.clone(),
    );

    sender.send_text("message", "title").await.unwrap();
    let payload: serde_json::Value =
        serde_json::from_slice(transport.last_request().body.as_deref().unwrap()).unwrap();
    assert_eq!(payload["channel"], "9|66");
    assert_eq!(payload["noip"], "1");
    assert!(payload.get("openid").is_none());
}

#[tokio::test]
async fn empty_provider_always_fails() {
    let error = EmptySender.send_text("x", "y").await.unwrap_err();
    assert!(error.to_string().contains("empty message sender"));
}

#[tokio::test]
async fn registry_exposes_all_go_providers_and_defaults() {
    let names = registry().names();
    for expected in [
        "telegram",
        "bark",
        "webhook",
        "email",
        "Server酱³",
        "Server酱Turbo",
        "empty",
        "Javascript",
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }

    let webhook = get_provider_configuration("webhook").unwrap();
    assert_eq!(webhook["body"], DEFAULT_BODY);
    let email = get_provider_configuration("email").unwrap();
    assert_eq!(email["port"], 587);
    let unknown = get_provider_configuration("nope").unwrap_err();
    assert!(matches!(unknown, NotifyError::ProviderNotFound(_)));
}

#[tokio::test]
async fn javascript_provider_is_explicitly_unsupported() {
    let error = match load_provider("Javascript", r#"{"script":"void 0"}"#).await {
        Ok(_) => panic!("Javascript provider must be unsupported"),
        Err(error) => error,
    };
    assert!(matches!(error, NotifyError::Unsupported(_)));
}

#[tokio::test]
async fn initialize_defaults_inserts_missing_rows_only() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();

    let inserted = initialize_defaults(&db).unwrap();
    assert_eq!(inserted, registry().names().len());
    assert!(db.message_senders().get("bark").unwrap().is_some());
    assert_eq!(initialize_defaults(&db).unwrap(), 0);
}
