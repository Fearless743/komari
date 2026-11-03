//! External API redaction tests.

use std::time::Duration;

use komari_notify::providers::bark::BarkConfig;
use komari_notify::providers::email::{EmailConfig, EmailSender};
use komari_notify::providers::javascript::JavaScriptConfig;
use komari_notify::providers::serverchan3::ServerChan3Config;
use komari_notify::providers::serverchanturbo::ServerChanTurboConfig;
use komari_notify::providers::telegram::TelegramConfig;
use komari_notify::providers::webhook::WebhookConfig;
use komari_notify::{
    HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, redact_addition, registry,
};

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

#[test]
fn webhook_url_key_variants_and_path_tokens_are_removed() {
    for addition in [
        r#"{"URL":"https://user:pass@hook.example/path"}"#,
        r#"{"Url":"https://hooks.slack.com/services/T000/B000/SLACK_SECRET"}"#,
        r#"{"url":"https://discord.com/api/webhooks/123/DISCORD_SECRET"}"#,
        r#"{"url":"https://outlook.office.com/webhook/id@tenant/IncomingWebhook/TEAMS_SECRET/id"}"#,
    ] {
        let safe = redact_addition("webhook", addition);
        assert!(
            safe.get("url").is_none() && safe.get("URL").is_none() && safe.get("Url").is_none(),
            "credentialed URL survived: {addition} -> {safe}"
        );
    }
}

#[test]
fn webhook_query_signatures_are_removed_but_unrelated_keys_remain() {
    for query in [
        "?sig=SIGNATURE_SECRET",
        "?signature=SIGNATURE_SECRET",
        "?X-Amz-Signature=SIGNATURE_SECRET",
        "?x-hub-signature-256=SIGNATURE_SECRET",
        "?access_token=ACCESS_SECRET",
        "?client_secret=CLIENT_SECRET",
        "?x-amz-credential=CREDENTIAL_SECRET",
        "?hmac=HMAC_SECRET",
    ] {
        let addition = format!(r#"{{"url":"https://hook.example/path{query}"}}"#);
        let safe = redact_addition("webhook", &addition);
        assert!(safe.get("url").is_none(), "{query} was not redacted");
    }

    let safe = redact_addition(
        "webhook",
        r#"{"url":"https://hook.example/path?author=alice&monkey=1"}"#,
    );
    assert_eq!(
        safe["url"],
        "https://hook.example/path?author=alice&monkey=1"
    );
}

#[test]
fn all_url_fields_are_checked_for_credentials() {
    let telegram = redact_addition(
        "telegram",
        r#"{"endpoint":"https://api.telegram.org/botTELEGRAM_SECRET","bot_token":"x"}"#,
    );
    assert!(telegram.get("endpoint").is_none());
    assert!(telegram.get("bot_token").is_none());

    let telegram_slash = redact_addition(
        "telegram",
        r#"{"endpoint":"https://api.telegram.org/bot/TELEGRAM_SECRET","bot_token":"x"}"#,
    );
    assert!(telegram_slash.get("endpoint").is_none());

    let telegram_default_slash = redact_addition(
        "telegram",
        r#"{"endpoint":"https://api.telegram.org/bot/","bot_token":"x"}"#,
    );
    assert_eq!(
        telegram_default_slash["endpoint"],
        "https://api.telegram.org/bot/"
    );

    let bark = redact_addition(
        "bark",
        r#"{"server_url":"https://user:pass@bark.example/path","device_key":"x"}"#,
    );
    assert!(bark.get("server_url").is_none());
    assert!(bark.get("device_key").is_none());

    let bark_path = redact_addition(
        "bark",
        r#"{"server_url":"https://bark.example/token/BARK_SECRET","device_key":"x"}"#,
    );
    assert!(bark_path.get("server_url").is_none());

    let custom_webhook = redact_addition(
        "webhook",
        r#"{"url":"https://hook.example/webhook/CUSTOM_SECRET"}"#,
    );
    assert!(custom_webhook.get("url").is_none());

    for path in ["/send/SECRET", "/key/SECRET", "/secret/SECRET"] {
        let addition = format!(r#"{{"url":"https://hook.example{path}"}}"#);
        let safe = redact_addition("webhook", &addition);
        assert!(safe.get("url").is_none(), "{path} was not redacted");
    }

    for path in ["/send/message", "/webhook/events", "/token/refresh"] {
        let addition = format!(r#"{{"url":"https://hook.example{path}"}}"#);
        let safe = redact_addition("webhook", &addition);
        assert_eq!(
            safe["url"],
            format!("https://hook.example{path}"),
            "{path} should be preserved"
        );
    }

    let plain = redact_addition(
        "bark",
        r#"{"server_url":"https://bark.example/path","device_key":"x"}"#,
    );
    assert_eq!(plain["server_url"], "https://bark.example/path");
}

#[test]
fn http_debug_hides_path_tokens_and_signature_headers() {
    let telegram = HttpRequest::post("https://api.telegram.org/bot123:TELEGRAM_SECRET/sendMessage");
    let debug = format!("{telegram:?}");
    assert!(!debug.contains("TELEGRAM_SECRET"));
    assert!(debug.contains("api.telegram.org"));

    let serverchan = HttpRequest::post("https://sc3.example/send/SERVERCHAN_SECRET.send");
    assert!(!format!("{serverchan:?}").contains("SERVERCHAN_SECRET"));

    let slack = HttpRequest::post("https://hooks.slack.com/services/T/B/SLACK_SECRET")
        .header("X-Auth", "X_AUTH_SECRET")
        .header("X-Hub-Signature-256", "HUB_SIGNATURE_SECRET")
        .header("X-Amz-Signature", "AMZ_SIGNATURE_SECRET")
        .header("X-Amz-Credential", "AMZ_CREDENTIAL_SECRET")
        .header("X-Amz-Security-Token", "AMZ_SECURITY_TOKEN_SECRET")
        .header("X-Goog-Credential", "GOOG_CREDENTIAL_SECRET")
        .header("X-Api-Key", "API_KEY_SECRET")
        .header("X-Authorization", "X_AUTHORIZATION_SECRET")
        .header(
            "X-Forwarded-Authorization",
            "FORWARDED_AUTHORIZATION_SECRET",
        )
        .header("hmac", "HMAC_SECRET")
        .header("sig", "SIG_SECRET")
        .header("X-Test", "visible");
    let debug = format!("{slack:?}");
    assert!(!debug.contains("SLACK_SECRET"));
    assert!(!debug.contains("X_AUTH_SECRET"));
    assert!(!debug.contains("HUB_SIGNATURE_SECRET"));
    assert!(!debug.contains("AMZ_SIGNATURE_SECRET"));
    assert!(!debug.contains("AMZ_CREDENTIAL_SECRET"));
    assert!(!debug.contains("AMZ_SECURITY_TOKEN_SECRET"));
    assert!(!debug.contains("GOOG_CREDENTIAL_SECRET"));
    assert!(!debug.contains("API_KEY_SECRET"));
    assert!(!debug.contains("X_AUTHORIZATION_SECRET"));
    assert!(!debug.contains("FORWARDED_AUTHORIZATION_SECRET"));
    assert!(!debug.contains("HMAC_SECRET"));
    assert!(!debug.contains("SIG_SECRET"));
    assert!(debug.contains("visible"));

    let response = HttpResponse::new(200, b"RESPONSE_BODY_SECRET".to_vec());
    assert!(!format!("{response:?}").contains("RESPONSE_BODY_SECRET"));
}

#[test]
fn debug_redacts_bark_url_and_keeps_serverchan_metadata() {
    let bark = BarkConfig {
        server_url: "https://user:pass@bark.example/SECRET_PATH".to_owned(),
        device_key: "BARK_DEVICE_SECRET".to_owned(),
        level: "critical".to_owned(),
        ..BarkConfig::default()
    };
    let debug = format!("{bark:?}");
    assert!(!debug.contains("BARK_DEVICE_SECRET"));
    assert!(!debug.contains("SECRET_PATH"));
    assert!(!debug.contains("pass"));
    assert!(debug.contains("bark.example"));
    assert!(debug.contains("critical"));

    let sc3 = ServerChan3Config {
        api_url: "https://sc3.example/send/API_SECRET.send".to_owned(),
        tags: "tag-a|tag-b".to_owned(),
    };
    let debug = format!("{sc3:?}");
    assert!(!debug.contains("API_SECRET"));
    assert!(debug.contains("tag-a|tag-b"));

    let turbo = ServerChanTurboConfig {
        api_url: "https://sct.example/API_SECRET.send".to_owned(),
        channel: "9|66".to_owned(),
        noip: "1".to_owned(),
        openid: "oid".to_owned(),
    };
    let debug = format!("{turbo:?}");
    assert!(!debug.contains("API_SECRET"));
    assert!(debug.contains("9|66"));

    let javascript = JavaScriptConfig {
        script: "JS_SCRIPT_SECRET".to_owned(),
    };
    assert!(!format!("{javascript:?}").contains("JS_SCRIPT_SECRET"));
}

#[test]
fn debug_redacts_webhook_and_telegram_urls() {
    let webhook = WebhookConfig {
        url: "https://hook.example/path?token=WEBHOOK_QUERY_SECRET".to_owned(),
        ..WebhookConfig::default()
    };
    let debug = format!("{webhook:?}");
    assert!(!debug.contains("WEBHOOK_QUERY_SECRET"));
    assert!(debug.contains("hook.example"));

    let telegram = TelegramConfig {
        endpoint: "https://api.telegram.org/bot/TELEGRAM_PATH_SECRET".to_owned(),
        bot_token: "TELEGRAM_TOKEN_SECRET".to_owned(),
        ..TelegramConfig::default()
    };
    let debug = format!("{telegram:?}");
    assert!(!debug.contains("TELEGRAM_PATH_SECRET"));
    assert!(!debug.contains("TELEGRAM_TOKEN_SECRET"));
    assert!(debug.contains("api.telegram.org"));
}

#[tokio::test]
async fn registry_type_errors_do_not_echo_values() {
    let error = match komari_notify::load_provider(
        "email",
        r#"{"host":"smtp.example","port":"PORT_SECRET"}"#,
    )
    .await
    {
        Ok(_) => panic!("type mismatch must fail"),
        Err(error) => error,
    };
    let text = error.to_string();
    assert!(text.contains("email"), "{text}");
    assert!(text.contains("port"), "{text}");
    assert!(!text.contains("PORT_SECRET"), "{text}");

    let error = match komari_notify::load_provider(
        "email",
        r#"{"host":"smtp.example","port":"PORT\"SECRET"}"#,
    )
    .await
    {
        Ok(_) => panic!("escaped quote type mismatch must fail"),
        Err(error) => error,
    };
    let text = error.to_string();
    assert!(text.contains("port"), "{text}");
    assert!(!text.contains("SECRET"), "{text}");
}

#[tokio::test]
async fn reqwest_timeout_errors_do_not_include_the_url() {
    use std::io::Read;
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        // Read the request, then deliberately never respond.
        let mut buffer = [0u8; 1024];
        let _ = stream.read(&mut buffer);
        std::thread::sleep(Duration::from_millis(500));
    });

    let transport = ReqwestTransport::new(Duration::from_millis(100));
    let error = transport
        .execute(HttpRequest::get(format!(
            "http://127.0.0.1:{port}/TIMEOUT_SECRET?token=QUERY_SECRET"
        )))
        .await
        .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("timeout failed"), "{text}");
    assert!(!text.contains("TIMEOUT_SECRET"), "{text}");
    assert!(!text.contains("QUERY_SECRET"), "{text}");
    server.join().unwrap();
}

#[tokio::test]
async fn reqwest_errors_do_not_include_the_url() {
    let transport = ReqwestTransport::new(Duration::from_millis(500));
    let error = transport
        .execute(HttpRequest::get(
            "http://127.0.0.1:1/botREQUEST_SECRET?token=QUERY_SECRET",
        ))
        .await
        .unwrap_err();
    let text = error.to_string();
    assert!(!text.contains("REQUEST_SECRET"), "{text}");
    assert!(!text.contains("QUERY_SECRET"), "{text}");
}
