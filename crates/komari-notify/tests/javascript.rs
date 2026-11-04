//! JavaScript provider execution tests.
//!
//! Every test drives a real `boa_engine` context; HTTP is replaced by
//! [`support::MockHttp`] so no network is required.

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use komari_db::LocalTime;
use komari_db::models::{Client, EventMessage};
use komari_notify::providers::javascript::{JavaScriptConfig, JavaScriptSender};
use komari_notify::{
    EventMessageSender, HttpRequest, HttpResponse, HttpTransport, MessageSender,
    NotificationService, NotificationSettings, NotifyError, Result,
};
use support::MockHttp;

fn sender(script: &str, transport: Arc<MockHttp>) -> JavaScriptSender {
    JavaScriptSender::with_transport(
        JavaScriptConfig {
            script: script.to_owned(),
        },
        transport,
    )
    .with_timeout(Duration::from_secs(5))
}

fn text_sender(script: &str) -> JavaScriptSender {
    sender(script, Arc::new(MockHttp::new(Vec::new())))
}

fn event() -> EventMessage {
    EventMessage {
        event: "Offline".to_owned(),
        clients: vec![
            Client {
                name: "alpha".to_owned(),
                uuid: "u-1".to_owned(),
                ..Client::default()
            },
            Client {
                name: String::new(),
                uuid: "u-2".to_owned(),
                ..Client::default()
            },
        ],
        time: LocalTime::parse("2026-01-02T03:04:05Z").unwrap(),
        message: "gone".to_owned(),
        emoji: "🔴".to_owned(),
    }
}

#[tokio::test]
async fn send_message_returning_true_succeeds() {
    let sender = text_sender("function sendMessage(message, title) { return true; }");
    sender.init().await.unwrap();
    sender.send_text("hello", "title").await.unwrap();
}

#[tokio::test]
async fn send_message_returning_false_fails() {
    let sender = text_sender("function sendMessage() { return false; }");
    let error = sender.send_text("hello", "title").await.unwrap_err();
    assert!(
        error.to_string().contains("sendMessage returned false"),
        "{error}"
    );
}

#[tokio::test]
async fn send_message_awaiting_a_promise_fulfilled_with_true_succeeds() {
    let sender = text_sender(
        r#"
        async function sendMessage(message, title) {
            await Promise.resolve();
            await new Promise(function (resolve) { setTimeout(resolve, 5); });
            return message === "hello" && title === "title";
        }
        "#,
    );
    sender.send_text("hello", "title").await.unwrap();
}

#[tokio::test]
async fn send_message_returning_a_rejected_promise_fails() {
    let sender = text_sender(
        r#"
        function sendMessage() {
            return Promise.reject(new Error("delivery failed"));
        }
        "#,
    );
    let error = sender.send_text("hello", "title").await.unwrap_err();
    assert!(error.to_string().contains("Promise rejected"), "{error}");
}

#[tokio::test]
async fn async_fetch_posts_the_expected_request_and_reads_json() {
    let transport = Arc::new(MockHttp::new(vec![
        HttpResponse::new(200, br#"{"ok":true}"#.to_vec())
            .with_header("content-type", "application/json"),
    ]));
    let sender = sender(
        r#"
        async function sendMessage(message, title) {
            const response = await fetch("https://hook.example/notify", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ message: message, title: title }),
            });
            const data = await response.json();
            return response.ok && data.ok === true;
        }
        "#,
        transport.clone(),
    );

    sender.send_text("hello", "title").await.unwrap();

    let request = transport.last_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, "https://hook.example/notify");
    assert_eq!(
        request.header_value("content-type"),
        Some("application/json")
    );
    assert_eq!(
        request.body_text().unwrap(),
        r#"{"message":"hello","title":"title"}"#
    );
}

#[tokio::test]
async fn fetch_response_exposes_status_status_text_headers_and_text() {
    let transport = Arc::new(MockHttp::new(vec![
        HttpResponse::new(201, b"hello".to_vec()).with_header("X-Test", "yes"),
    ]));
    let sender = sender(
        r#"
        async function sendMessage() {
            const response = await fetch("https://hook.example/created");
            const text = await response.text();
            return response.status === 201
                && response.statusText === "201 Created"
                && response.ok === true
                && response.headers.get("x-test") === "yes"
                && text === "hello";
        }
        "#,
        transport,
    );

    sender.send_text("ignored", "ignored").await.unwrap();
}

#[tokio::test]
async fn fetch_rejection_fails_without_leaking_the_url() {
    let sender = JavaScriptSender::with_transport(
        JavaScriptConfig {
            script: r#"
                async function sendMessage() {
                    const response = await fetch("https://hook.example/secret-token-URL_SECRET");
                    return response.ok;
                }
            "#
            .to_owned(),
        },
        Arc::new(FailingTransport),
    )
    .with_timeout(Duration::from_secs(5));

    let error = sender.send_text("hello", "title").await.unwrap_err();
    assert!(error.to_string().contains("Promise rejected"), "{error}");
    assert!(!error.to_string().contains("URL_SECRET"), "{error}");
}

#[tokio::test]
async fn xml_http_request_delivers_response_and_handlers() {
    let transport = Arc::new(MockHttp::new(vec![
        HttpResponse::new(200, b"pong".to_vec()).with_header("X-Test", "yes"),
    ]));
    let sender = sender(
        r#"
        function sendMessage() {
            return new Promise(function (resolve, reject) {
                const xhr = new XMLHttpRequest();
                xhr.open("POST", "https://hook.example/xhr");
                xhr.setRequestHeader("X-Test", "yes");
                xhr.onreadystatechange = function () {
                    if (xhr.readyState === 4) {
                        resolve(xhr.status === 200 && xhr.responseText === "pong" && xhr.response === "pong");
                    }
                };
                xhr.onerror = function () { reject(new Error("xhr failed")); };
                xhr.send("ping");
            });
        }
        "#,
        transport.clone(),
    );

    sender.send_text("hello", "title").await.unwrap();

    let request = transport.last_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, "https://hook.example/xhr");
    assert_eq!(request.header_value("X-Test"), Some("yes"));
    assert_eq!(request.body_text().unwrap(), "ping");
}

#[tokio::test]
async fn xml_http_request_onerror_fires_when_the_transport_fails() {
    let sender = JavaScriptSender::with_transport(
        JavaScriptConfig {
            script: r#"
                function sendMessage() {
                    return new Promise(function (resolve) {
                        const order = [];
                        const xhr = new XMLHttpRequest();
                        xhr.open("GET", "https://hook.example/xhr");
                        xhr.onerror = function () { order.push("error"); };
                        xhr.onreadystatechange = function () {
                            if (xhr.readyState !== 4) return;
                            order.push("readystatechange");
                            resolve(
                                order.join(",") === "error,readystatechange"
                                    && xhr.status === 0
                                    && xhr.statusText === "connect failed"
                            );
                        };
                        xhr.send();
                    });
                }
            "#
            .to_owned(),
        },
        Arc::new(FailingTransport),
    )
    .with_timeout(Duration::from_secs(5));

    sender.send_text("hello", "title").await.unwrap();
}

#[tokio::test]
async fn synchronous_xml_http_request_failure_calls_onerror() {
    let sender = JavaScriptSender::with_transport(
        JavaScriptConfig {
            script: r#"
                function sendMessage() {
                    const order = [];
                    const xhr = new XMLHttpRequest();
                    xhr.open("GET", "https://hook.example/xhr", false);
                    xhr.onerror = function () { order.push("error"); };
                    xhr.onreadystatechange = function () {
                        if (xhr.readyState === 4) order.push("readystatechange");
                    };
                    xhr.send();
                    return order.join(",") === "error,readystatechange"
                        && xhr.readyState === 4
                        && xhr.status === 0
                        && xhr.statusText === "connect failed";
                }
            "#
            .to_owned(),
        },
        Arc::new(FailingTransport),
    )
    .with_timeout(Duration::from_secs(5));

    sender.send_text("hello", "title").await.unwrap();
}

#[tokio::test]
async fn fetch_rejects_with_a_string_like_go() {
    let sender = JavaScriptSender::with_transport(
        JavaScriptConfig {
            script: r#"
                async function sendMessage() {
                    try {
                        await fetch("https://hook.example/x");
                        return false;
                    } catch (error) {
                        return typeof error === "string"
                            && error.indexOf("Fetch failed: connect failed") === 0;
                    }
                }
            "#
            .to_owned(),
        },
        Arc::new(FailingTransport),
    )
    .with_timeout(Duration::from_secs(5));

    sender.send_text("hello", "title").await.unwrap();
}

#[tokio::test]
async fn fetch_uses_the_wire_reason_phrase_when_available() {
    let transport = Arc::new(MockHttp::new(vec![
        HttpResponse::new(202, b"ok".to_vec()).with_status_text("Custom Reason"),
    ]));
    let sender = sender(
        r#"
        async function sendMessage() {
            const response = await fetch("https://hook.example/custom");
            return response.status === 202 && response.statusText === "202 Custom Reason";
        }
        "#,
        transport,
    );

    sender.send_text("hello", "title").await.unwrap();
}

#[tokio::test]
async fn synchronous_xml_http_request_completes_before_send_returns() {
    let transport = Arc::new(MockHttp::new(vec![HttpResponse::new(
        200,
        b"pong".to_vec(),
    )]));
    let sender = sender(
        r#"
        function sendMessage() {
            const xhr = new XMLHttpRequest();
            xhr.open("GET", "https://hook.example/sync", false);
            xhr.send();
            return xhr.readyState === 4 && xhr.status === 200 && xhr.responseText === "pong";
        }
        "#,
        transport,
    );

    sender.send_text("hello", "title").await.unwrap();
}

#[tokio::test]
async fn never_settling_promise_hits_the_deadline() {
    let sender = text_sender("function sendMessage() { return new Promise(function () {}); }")
        .with_timeout(Duration::from_millis(100));

    let error = sender.send_text("hello", "title").await.unwrap_err();
    assert!(error.to_string().contains("timeout"), "{error}");
}

#[tokio::test]
async fn synchronous_send_message_hits_the_wall_clock_deadline() {
    // Go only checks the timeout while polling a Promise, so a synchronous
    // loop hangs forever there. The Rust provider bounds the whole call.
    let sender = text_sender("function sendMessage() { while (true) {} }")
        .with_timeout(Duration::from_millis(100));

    let started = std::time::Instant::now();
    let error = sender.send_text("hello", "title").await.unwrap_err();
    let elapsed = started.elapsed();

    assert!(error.to_string().contains("timeout"), "{error}");
    assert!(elapsed < Duration::from_secs(3), "timeout took {elapsed:?}");
}

#[tokio::test]
async fn script_load_time_is_not_counted_by_the_deadline() {
    // Go starts its 30s timer in SendTextMessage, after Init ran the script.
    // Loading takes ~300ms here while the invocation budget is 100ms.
    let sender = text_sender(
        r#"
        const started = Date.now();
        while (Date.now() - started < 300) {}
        function sendMessage() { return true; }
        "#,
    )
    .with_timeout(Duration::from_millis(100));

    sender.send_text("hello", "title").await.unwrap();
}

#[tokio::test]
async fn top_level_state_persists_between_messages() {
    // First call sees calls === 1 and fails; a fresh VM would also see 1.
    let sender = text_sender(
        r#"
        var calls = 0;
        function sendMessage() {
            calls += 1;
            return calls !== 1;
        }
        "#,
    );

    let error = sender.send_text("hello", "title").await.unwrap_err();
    assert!(
        error.to_string().contains("sendMessage returned false"),
        "{error}"
    );
    sender.send_text("hello", "title").await.unwrap();
}

#[tokio::test]
async fn top_level_code_runs_once_for_init_and_send() {
    let sender = text_sender(
        r#"
        var runs = 0;
        runs += 1;
        function sendMessage() { return runs === 1; }
        "#,
    );

    sender.init().await.unwrap();
    sender.send_text("hello", "title").await.unwrap();
}

#[tokio::test]
async fn syntax_error_is_rejected_without_echoing_the_script() {
    let sender = text_sender("const JS_LEAK_SECRET = 'x'; function (");
    let error = sender.init().await.unwrap_err();
    assert!(matches!(error, NotifyError::InvalidConfig(_)), "{error:?}");
    let text = error.to_string();
    assert!(text.contains("failed to load JavaScript script"), "{text}");
    assert!(!text.contains("JS_LEAK_SECRET"), "{text}");
}

#[tokio::test]
async fn missing_send_message_is_reported_like_go() {
    let sender = text_sender("var other = 1;");
    let error = sender.init().await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("sendMessage function not defined in script"),
        "{error}"
    );
}

#[tokio::test]
async fn non_function_send_message_is_reported_like_go() {
    let sender = text_sender("var sendMessage = 42;");
    let error = sender.init().await.unwrap_err();
    assert!(
        error.to_string().contains("sendMessage is not a function"),
        "{error}"
    );
}

#[tokio::test]
async fn empty_script_is_rejected() {
    let error = text_sender("").init().await.unwrap_err();
    assert!(
        error.to_string().contains("JavaScript script is empty"),
        "{error}"
    );
}

#[tokio::test]
async fn send_event_uses_the_send_event_function() {
    let sender = text_sender(
        r#"
        function sendMessage() { return false; }
        function sendEvent(event) {
            return event.event === "Offline"
                && event.emoji === "🔴"
                && event.clients.length === 2
                && event.clients[1].uuid === "u-2"
                && event.time === "2026-01-02T03:04:05Z";
        }
        "#,
    );

    sender.send_event(&event()).await.unwrap();
}

#[tokio::test]
async fn send_event_falls_back_to_send_message() {
    let sender = text_sender(
        r#"
        function sendMessage(message, title) {
            return title === "Offline"
                && message.indexOf("Clients: [alpha u-2]") >= 0
                && message.indexOf("Event: Offline") >= 0;
        }
        "#,
    );

    sender.send_event(&event()).await.unwrap();
}

#[tokio::test]
async fn non_callable_send_event_falls_back_to_send_message() {
    let sender = text_sender(
        r#"
        var sendEvent = 7;
        function sendMessage(message, title) { return message.indexOf("Event: Offline") >= 0; }
        "#,
    );

    sender.send_event(&event()).await.unwrap();
}

#[tokio::test]
async fn thrown_error_does_not_leak_the_secret() {
    let sender = text_sender(
        r#"
        const TOKEN = "JS_RUNTIME_SECRET";
        function sendMessage() { throw new Error("failed with " + TOKEN); }
        "#,
    );

    let error = sender.send_text("hello", "title").await.unwrap_err();
    let text = error.to_string();
    assert!(text.contains("JavaScript error"), "{text}");
    assert!(!text.contains("JS_RUNTIME_SECRET"), "{text}");
}

#[tokio::test]
async fn rejected_promise_does_not_leak_the_secret() {
    let sender = text_sender(
        r#"
        const TOKEN = "JS_REJECT_SECRET";
        function sendMessage() { return Promise.reject(new Error(TOKEN)); }
        "#,
    );

    let error = sender.send_text("hello", "title").await.unwrap_err();
    let text = error.to_string();
    assert!(text.contains("Promise rejected"), "{text}");
    assert!(!text.contains("JS_REJECT_SECRET"), "{text}");
}

#[tokio::test]
async fn require_supports_goja_builtins_and_rejects_unknown_modules() {
    let sender = text_sender(
        r#"
        function sendMessage() {
            const consoleModule = require("console");
            const processModule = require("process");
            consoleModule.log("script log");
            let unknown = false;
            try {
                require("fs");
            } catch (error) {
                unknown = String(error.message).indexOf("Cannot find module 'fs'") >= 0;
            }
            return typeof consoleModule.log === "function"
                && typeof processModule.version === "string"
                && unknown;
        }
        "#,
    );

    sender.send_text("hello", "title").await.unwrap();
}

#[tokio::test]
async fn send_event_errors_are_not_silently_fallen_back() {
    let sender = text_sender(
        r#"
        function sendMessage() { return true; }
        function sendEvent() { return false; }
        "#,
    );

    let error = sender.send_event(&event()).await.unwrap_err();
    assert!(
        error.to_string().contains("sendEvent returned false"),
        "{error}"
    );
}

#[tokio::test]
async fn notification_service_prefers_the_event_sender() {
    let sender: Arc<dyn MessageSender> = Arc::new(text_sender(
        r#"
        function sendMessage() { return false; }
        function sendEvent(event) { return event.event === "Offline"; }
        "#,
    ));
    let service = NotificationService::with_sender(
        sender,
        NotificationSettings {
            enabled: true,
            template: "unused {{event}}".to_owned(),
        },
    );

    assert!(service.send_event(&event()).await.unwrap());
}

struct FailingTransport;

#[async_trait]
impl HttpTransport for FailingTransport {
    async fn execute(&self, _request: HttpRequest) -> Result<HttpResponse> {
        Err(NotifyError::Http("connect failed".to_owned()))
    }
}
