//! Provider request-construction and configuration tests.

mod support;

use std::sync::Arc;

use komari_db::Db;
use komari_notify::providers::bark::{BarkConfig, BarkSender};
use komari_notify::providers::email::{
    EmailConfig, EmailSender, EmailTransport, LettreEmailTransport, TlsMode, tls_mode,
};
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
    assert!(error.to_string().contains("telegram API error"));
    assert!(!error.to_string().contains("chat not found"));
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
            body: DEFAULT_BODY.to_owned(),
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
    assert!(!error.to_string().contains("boom"));
}

#[tokio::test]
async fn remote_response_bodies_are_not_echoed_in_errors() {
    let telegram = TelegramSender::with_transport(
        TelegramConfig {
            bot_token: "t".to_owned(),
            chat_id: "1".to_owned(),
            ..TelegramConfig::default()
        },
        Arc::new(MockHttp::with_json(&serde_json::json!({
            "ok": false,
            "description": "RESPONSE_SECRET"
        }))),
    );
    let error = telegram.send_text("hello", "").await.unwrap_err();
    assert!(!error.to_string().contains("RESPONSE_SECRET"));

    let bark = bark_sender(HttpResponse::new(
        200,
        br#"{"code":500,"message":"RESPONSE_SECRET"}"#.to_vec(),
    ));
    let error = bark.send_text("body", "title").await.unwrap_err();
    assert!(!error.to_string().contains("RESPONSE_SECRET"));

    let webhook = WebhookSender::with_transport(
        WebhookConfig {
            url: "https://hook.example".to_owned(),
            method: "POST".to_owned(),
            ..WebhookConfig::default()
        },
        Arc::new(MockHttp::new(vec![HttpResponse::new(
            500,
            b"RESPONSE_SECRET".to_vec(),
        )])),
    );
    let error = webhook.send_text("x", "y").await.unwrap_err();
    assert!(!error.to_string().contains("RESPONSE_SECRET"));

    let sc3 = ServerChan3Sender::with_transport(
        ServerChan3Config {
            api_url: "https://sc3.example/send/key.send".to_owned(),
            ..ServerChan3Config::default()
        },
        Arc::new(MockHttp::new(vec![HttpResponse::new(
            500,
            b"RESPONSE_SECRET".to_vec(),
        )])),
    );
    let error = sc3.send_text("x", "y").await.unwrap_err();
    assert!(!error.to_string().contains("RESPONSE_SECRET"));

    let turbo = ServerChanTurboSender::with_transport(
        ServerChanTurboConfig {
            api_url: "https://sct.example/key.send".to_owned(),
            ..ServerChanTurboConfig::default()
        },
        Arc::new(MockHttp::new(vec![HttpResponse::new(
            500,
            b"RESPONSE_SECRET".to_vec(),
        )])),
    );
    let error = turbo.send_text("x", "y").await.unwrap_err();
    assert!(!error.to_string().contains("RESPONSE_SECRET"));
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
    assert!(raw.contains("Content-Transfer-Encoding: quoted-printable"));
    assert!(raw.contains("hello"));
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

    // Go serialises the zero-value Addition; UI defaults live in fields().
    let webhook = get_provider_configuration("webhook").unwrap();
    assert_eq!(webhook["body"], "");
    assert_eq!(webhook["method"], "");
    assert_eq!(webhook["content_type"], "");
    let email = get_provider_configuration("email").unwrap();
    assert_eq!(email["port"], 0);
    let bark = get_provider_configuration("bark").unwrap();
    assert_eq!(bark["server_url"], "");
    assert_eq!(bark["level"], "");
    let telegram = get_provider_configuration("telegram").unwrap();
    assert_eq!(telegram["endpoint"], "");

    let fields = registry().config_fields();
    assert_eq!(field_default(&fields, "webhook", "body"), DEFAULT_BODY);
    assert_eq!(field_default(&fields, "webhook", "method"), "GET");
    assert_eq!(field_default(&fields, "email", "port"), "587");
    assert_eq!(
        field_default(&fields, "telegram", "endpoint"),
        "https://api.telegram.org/bot"
    );
    assert_eq!(field_default(&fields, "bark", "level"), "timeSensitive");

    let unknown = get_provider_configuration("nope").unwrap_err();
    assert!(matches!(unknown, NotifyError::ProviderNotFound(_)));
}

#[tokio::test]
async fn javascript_provider_loads_user_scripts() {
    let provider = load_provider(
        "Javascript",
        r#"{"script":"function sendMessage(message, title) { return true; }"}"#,
    )
    .await
    .unwrap();
    assert_eq!(provider.name(), "Javascript");
    provider.send_text("hello", "title").await.unwrap();

    // Go's Init rejects scripts without a callable sendMessage.
    let error = match load_provider("Javascript", r#"{"script":"void 0"}"#).await {
        Ok(_) => panic!("scripts without sendMessage must be rejected"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("sendMessage function not defined in script"),
        "{error}"
    );
}

#[tokio::test]
async fn initialize_defaults_inserts_missing_rows_only() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();

    let inserted = initialize_defaults(&db).unwrap();
    assert_eq!(inserted, registry().names().len());
    let bark = db.message_senders().get("bark").unwrap().unwrap();
    let bark_config: serde_json::Value = serde_json::from_str(&bark.addition).unwrap();
    assert_eq!(bark_config["server_url"], "");
    assert_eq!(bark_config["level"], "");
    let webhook = db.message_senders().get("webhook").unwrap().unwrap();
    let webhook_config: serde_json::Value = serde_json::from_str(&webhook.addition).unwrap();
    assert_eq!(webhook_config["body"], "");
    let email = db.message_senders().get("email").unwrap().unwrap();
    let email_config: serde_json::Value = serde_json::from_str(&email.addition).unwrap();
    assert_eq!(email_config["port"], 0);
    assert_eq!(initialize_defaults(&db).unwrap(), 0);
}

fn field_default(
    fields: &std::collections::HashMap<&'static str, Vec<komari_notify::ConfigField>>,
    provider: &str,
    key: &str,
) -> String {
    fields[provider]
        .iter()
        .find(|field| field.name == key)
        .expect("field exists")
        .default
        .clone()
}

#[tokio::test]
async fn registry_accepts_go_null_addition_forms() {
    let top_level_null = load_provider("telegram", "null").await.unwrap();
    assert_eq!(top_level_null.config()["endpoint"], "");

    let missing_fields = load_provider("telegram", "{}").await.unwrap();
    assert_eq!(missing_fields.config()["endpoint"], "");

    let field_null = load_provider(
        "telegram",
        r#"{"bot_token":"x","chat_id":null,"endpoint":null}"#,
    )
    .await
    .unwrap();
    assert_eq!(field_null.config()["bot_token"], "x");
    assert_eq!(field_null.config()["chat_id"], "");
    assert_eq!(field_null.config()["endpoint"], "");
}

#[tokio::test]
async fn registry_rejects_empty_addition_like_go() {
    let error = match load_provider("telegram", "").await {
        Ok(_) => panic!("empty addition must fail"),
        Err(error) => error,
    };
    assert!(matches!(error, NotifyError::InvalidConfig(_)));
    assert!(error.to_string().contains("unexpected end of JSON"));
}

fn bark_sender(response: HttpResponse) -> BarkSender {
    BarkSender::with_transport(
        BarkConfig {
            device_key: "device".to_owned(),
            ..BarkConfig::default()
        },
        Arc::new(MockHttp::new(vec![response])),
    )
}

#[tokio::test]
async fn bark_decodes_null_and_missing_code_as_zero() {
    for body in ["{}", "null", r#"{"code":null}"#, r#"{"message":"no code"}"#] {
        let sender = bark_sender(HttpResponse::new(200, body.as_bytes().to_vec()));
        let error = sender.send_text("body", "title").await.unwrap_err();
        assert!(error.to_string().contains("code: 0"), "{body}");
    }

    bark_sender(HttpResponse::new(200, br#"{"code":200}"#.to_vec()))
        .send_text("body", "title")
        .await
        .unwrap();
    // Non-JSON remains a success for self-hosted Bark servers.
    bark_sender(HttpResponse::new(200, b"ok".to_vec()))
        .send_text("body", "title")
        .await
        .unwrap();
}

#[test]
fn email_tls_mode_matches_go() {
    assert_eq!(tls_mode(false, 587), TlsMode::Opportunistic);
    assert_eq!(tls_mode(true, 465), TlsMode::Wrapper);
    assert_eq!(tls_mode(true, 587), TlsMode::Required);
}

#[tokio::test]
async fn email_body_is_quoted_printable_and_subject_is_rfc2047() {
    let sender = EmailSender::with_transport(
        EmailConfig {
            host: "smtp.example".to_owned(),
            port: 587,
            username: "user".to_owned(),
            password: "pass".to_owned(),
            sender: "bot@example.com".to_owned(),
            receiver: "a@example.com".to_owned(),
            ..EmailConfig::default()
        },
        Arc::new(MockEmail::new()),
    );
    let envelope = sender.build_envelope("a=b 中文", "状态提醒").unwrap();
    let raw = envelope.raw_text();
    assert!(raw.contains("Content-Transfer-Encoding: quoted-printable"));
    assert!(raw.contains("=3D"), "body should be QP encoded: {raw}");
    assert!(raw.contains("=?utf-8?"), "subject should be RFC2047: {raw}");
}

#[tokio::test]
async fn email_without_ssl_attempts_starttls_when_advertised() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        writer.write_all(b"220 mock smtp\r\n").unwrap();
        let mut saw_starttls = false;
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let command = line.trim().to_ascii_uppercase();
            if command.starts_with("EHLO") {
                writer
                    .write_all(b"250-mock\r\n250-STARTTLS\r\n250 OK\r\n")
                    .unwrap();
            } else if command.starts_with("STARTTLS") {
                saw_starttls = true;
                let _ = writer.write_all(b"220 go ahead\r\n");
                break;
            } else if command.starts_with("QUIT") {
                break;
            } else {
                let _ = writer.write_all(b"250 OK\r\n");
            }
        }
        saw_starttls
    });

    let sender = EmailSender::with_transport(
        EmailConfig {
            host: "127.0.0.1".to_owned(),
            port: i64::from(port),
            username: "user".to_owned(),
            password: "pass".to_owned(),
            sender: "bot@example.com".to_owned(),
            receiver: "a@example.com".to_owned(),
            ..EmailConfig::default()
        },
        Arc::new(LettreEmailTransport),
    );
    let envelope = sender.build_envelope("hello", "title").unwrap();
    // The handshake cannot complete against the mock, but the client must
    // have issued STARTTLS rather than continuing in plaintext.
    let _ = LettreEmailTransport.send(envelope).await;
    assert!(server.join().unwrap(), "STARTTLS was not attempted");
}
