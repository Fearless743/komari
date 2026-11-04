//! End-to-end check that `fetch` works through the production transport.
//!
//! The other JavaScript tests replace HTTP with `support::MockHttp`; this one
//! exercises `ReqwestTransport`, including response-header extraction, against
//! a loopback server.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::Duration;

use komari_notify::MessageSender;
use komari_notify::providers::javascript::{JavaScriptConfig, JavaScriptSender};

#[tokio::test]
async fn fetch_roundtrips_through_the_reqwest_transport() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);

        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut saw_token_header = false;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
            if line.to_ascii_lowercase().starts_with("x-token:") {
                saw_token_header = true;
            }
        }

        writer
            .write_all(
                b"HTTP/1.1 202 Accepted\r\n\
                  Content-Type: application/json\r\n\
                  X-Reply: yes\r\n\
                  Content-Length: 11\r\n\
                  \r\n\
                  {\"ok\":true}",
            )
            .unwrap();
        (request_line, saw_token_header)
    });

    let sender = JavaScriptSender::new(JavaScriptConfig {
        script: format!(
            r#"
            async function sendMessage(message, title) {{
                const response = await fetch("http://127.0.0.1:{port}/notify", {{
                    method: "POST",
                    headers: {{ "X-Token": "abc" }},
                    body: "hello",
                }});
                const data = await response.json();
                return response.status === 202
                    && response.statusText === "202 Accepted"
                    && response.headers["Content-Type"] === "application/json"
                    && response.headers["X-Reply"] === "yes"
                    && response.headers.get("x-reply") === "yes"
                    && data.ok === true;
            }}
            "#
        ),
    })
    .with_timeout(Duration::from_secs(10));

    sender.send_text("hello", "title").await.unwrap();

    let (request_line, saw_token_header) = server.join().unwrap();
    assert!(request_line.starts_with("POST /notify"), "{request_line}");
    assert!(saw_token_header, "custom request header was not sent");
}
