//! External API redaction tests.

use komari_notify::providers::email::{EmailConfig, EmailSender};
use komari_notify::providers::javascript::JavaScriptConfig;
use komari_notify::providers::telegram::TelegramConfig;
use komari_notify::providers::webhook::WebhookConfig;
use komari_notify::{HttpRequest, redact_addition, registry};

#[test]
fn telegram_removes_bot_token_only() {
    let addition = r#"{"bot_token":"tg-secret","chat_id":"42","message_thread_id":"7","endpoint":"https://api.telegram.org/bot"}"#;
    let original = addition.to_owned();
    let safe = redact_addition("telegram", addition);

    assert!(safe.get("bot_token").is_none());
    assert_eq!(safe["chat_id"], "42");
    assert_eq!(safe["message_thread_id"], "7");
    assert_eq!(safe["endpoint"], "https://api.telegram.org/bot");
    assert_eq!(addition, original);
}

#[test]
fn bark_removes_device_key_only() {
    let safe = redact_addition(
        "bark",
        r#"{"server_url":"https://api.day.app","device_key":"bark-secret","icon":"i","level":"critical"}"#,
    );
    assert!(safe.get("device_key").is_none());
    assert_eq!(safe["server_url"], "https://api.day.app");
    assert_eq!(safe["icon"], "i");
    assert_eq!(safe["level"], "critical");
}

#[test]
fn serverchan_providers_remove_api_url_and_token_aliases() {
    let sc3 = redact_addition(
        "Server酱³",
        r#"{"api_url":"https://sc3.example/send/SENDKEY.send","sendkey":"x","send_key":"y","token":"z","tags":"a|b"}"#,
    );
    assert!(sc3.get("api_url").is_none());
    assert!(sc3.get("sendkey").is_none());
    assert!(sc3.get("send_key").is_none());
    assert!(sc3.get("token").is_none());
    assert_eq!(sc3["tags"], "a|b");

    let turbo = redact_addition(
        "Server酱Turbo",
        r#"{"api_url":"https://sctapi.ftqq.com/SENDKEY.send","channel":"9|66","noip":"1","openid":"oid"}"#,
    );
    assert!(turbo.get("api_url").is_none());
    assert_eq!(turbo["channel"], "9|66");
    assert_eq!(turbo["noip"], "1");
    assert_eq!(turbo["openid"], "oid");
}

#[test]
fn webhook_removes_headers_body_password_and_credentialed_url() {
    let safe = redact_addition(
        "webhook",
        r#"{"url":"https://user:pass@hook.example/path","method":"POST","content_type":"application/json","headers":"{\"Authorization\":\"Bearer x\"}","body":"{\"secret\":\"x\"}","username":"user","password":"pass"}"#,
    );
    assert!(safe.get("headers").is_none());
    assert!(safe.get("body").is_none());
    assert!(safe.get("password").is_none());
    assert!(safe.get("url").is_none());
    assert_eq!(safe["method"], "POST");
    assert_eq!(safe["content_type"], "application/json");
    assert_eq!(safe["username"], "user");
}

#[test]
fn webhook_keeps_url_without_credentials_and_removes_token_query() {
    let plain = redact_addition("webhook", r#"{"url":"https://hook.example/path"}"#);
    assert_eq!(plain["url"], "https://hook.example/path");

    let token_url = redact_addition(
        "webhook",
        r#"{"url":"https://hook.example/path?token=secret"}"#,
    );
    assert!(token_url.get("url").is_none());
}

#[test]
fn email_removes_password_only() {
    let safe = redact_addition(
        "email",
        r#"{"host":"smtp.example","port":587,"username":"user","password":"smtp-secret","sender":"bot@example.com","receiver":"a@example.com","use_ssl":true}"#,
    );
    assert!(safe.get("password").is_none());
    assert_eq!(safe["host"], "smtp.example");
    assert_eq!(safe["port"], 587);
    assert_eq!(safe["username"], "user");
    assert_eq!(safe["sender"], "bot@example.com");
    assert_eq!(safe["use_ssl"], true);
}

#[test]
fn javascript_removes_script_and_code_aliases() {
    let safe = redact_addition(
        "Javascript",
        r#"{"script":"fetch('https://secret')","code":"also secret","note":"kept"}"#,
    );
    assert!(safe.get("script").is_none());
    assert!(safe.get("code").is_none());
    assert_eq!(safe["note"], "kept");
}

#[test]
fn empty_provider_has_no_sensitive_fields() {
    let safe = redact_addition("empty", r#"{"anything":"kept"}"#);
    assert_eq!(safe["anything"], "kept");
}

#[test]
fn unknown_provider_and_malformed_values_are_safe() {
    assert_eq!(
        redact_addition("nope", r#"{"token":"secret"}"#),
        serde_json::json!({})
    );
    assert_eq!(
        redact_addition("telegram", "not json"),
        serde_json::json!({})
    );
    assert_eq!(redact_addition("telegram", "null"), serde_json::json!({}));
    assert_eq!(redact_addition("telegram", "[]"), serde_json::json!({}));
    assert_eq!(
        redact_addition("telegram", r#""secret""#),
        serde_json::json!({})
    );
    assert_eq!(redact_addition("telegram", "42"), serde_json::json!({}));
}

#[test]
fn sensitive_keys_are_normalized_and_do_not_leak() {
    let safe = redact_addition(
        "telegram",
        r#"{"bot_token":"a","BotToken":"b","bot-token":"c","chat_id":"1"}"#,
    );
    assert!(safe.get("bot_token").is_none());
    assert!(safe.get("BotToken").is_none());
    assert!(safe.get("bot-token").is_none());
    assert_eq!(safe["chat_id"], "1");
}

#[test]
fn registry_declares_the_same_sensitive_fields() {
    assert_eq!(
        registry().sensitive_fields("telegram"),
        Some(&["bot_token"][..])
    );
    assert_eq!(
        registry().sensitive_fields("bark"),
        Some(&["device_key"][..])
    );
    assert_eq!(registry().sensitive_fields("empty"), Some(&[][..]));
    assert_eq!(registry().sensitive_fields("unknown"), None);
}

#[test]
fn debug_output_does_not_contain_provider_secrets() {
    let telegram = TelegramConfig {
        bot_token: "TG_DEBUG_SECRET".to_owned(),
        chat_id: "42".to_owned(),
        ..TelegramConfig::default()
    };
    let debug = format!("{telegram:?}");
    assert!(!debug.contains("TG_DEBUG_SECRET"));
    assert!(debug.contains("42"));

    let webhook = WebhookConfig {
        headers: r#"{"Authorization":"WEBHOOK_HEADER_SECRET"}"#.to_owned(),
        body: r#"{"token":"WEBHOOK_BODY_SECRET"}"#.to_owned(),
        password: "WEBHOOK_PASSWORD_SECRET".to_owned(),
        ..WebhookConfig::default()
    };
    let debug = format!("{webhook:?}");
    assert!(!debug.contains("WEBHOOK_HEADER_SECRET"));
    assert!(!debug.contains("WEBHOOK_BODY_SECRET"));
    assert!(!debug.contains("WEBHOOK_PASSWORD_SECRET"));

    let email = EmailConfig {
        password: "EMAIL_PASSWORD_SECRET".to_owned(),
        ..EmailConfig::default()
    };
    assert!(!format!("{email:?}").contains("EMAIL_PASSWORD_SECRET"));

    let javascript = JavaScriptConfig {
        script: "JAVASCRIPT_SECRET".to_owned(),
    };
    assert!(!format!("{javascript:?}").contains("JAVASCRIPT_SECRET"));
}

#[test]
fn debug_output_does_not_contain_http_or_email_envelope_secrets() {
    let request = HttpRequest::post("https://user:pass@example.com/path?token=query-secret")
        .header("Authorization", "Bearer header-secret")
        .header("X-Test", "visible")
        .body("body-secret");
    let debug = format!("{request:?}");
    assert!(!debug.contains("pass"));
    assert!(!debug.contains("query-secret"));
    assert!(!debug.contains("header-secret"));
    assert!(!debug.contains("body-secret"));
    assert!(debug.contains("visible"));

    let sender = EmailSender::new(EmailConfig {
        host: "smtp.example".to_owned(),
        port: 587,
        username: "user".to_owned(),
        password: "ENVELOPE_PASSWORD_SECRET".to_owned(),
        sender: "bot@example.com".to_owned(),
        receiver: "a@example.com".to_owned(),
        ..EmailConfig::default()
    });
    let envelope = sender.build_envelope("mail body secret", "title").unwrap();
    let debug = format!("{envelope:?}");
    assert!(!debug.contains("ENVELOPE_PASSWORD_SECRET"));
    assert!(!debug.contains("mail body secret"));
}

#[test]
fn redaction_does_not_modify_the_database_row() {
    let db = komari_db::Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    let addition = r#"{"bot_token":"db-secret","chat_id":"42"}"#;
    db.message_senders()
        .save(&komari_db::models::MessageSenderProvider::new(
            "telegram", addition,
        ))
        .unwrap();

    let safe = redact_addition(
        "telegram",
        &db.message_senders()
            .get("telegram")
            .unwrap()
            .unwrap()
            .addition,
    );
    assert!(safe.get("bot_token").is_none());

    let stored = db.message_senders().get("telegram").unwrap().unwrap();
    assert_eq!(stored.addition, addition);
    assert!(stored.addition.contains("db-secret"));
}

#[tokio::test]
async fn malformed_addition_error_does_not_echo_input() {
    let error = match komari_notify::load_provider("telegram", "TOKEN_SHOULD_NOT_APPEAR").await {
        Ok(_) => panic!("malformed addition must fail"),
        Err(error) => error,
    };
    assert!(!error.to_string().contains("TOKEN_SHOULD_NOT_APPEAR"));
}
