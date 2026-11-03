mod common;

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use common::{app, app_with, call, create_client, default_config, records_for, report_body};
use futures_util::{SinkExt, StreamExt};
use komari_api::{ApiConfig, ClientEventKind, PresenceSource};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::ORIGIN;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn spawn(app: Router) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (address, handle)
}

async fn next_text(socket: &mut Socket) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .expect("websocket message timeout")
            .expect("websocket message")
            .expect("valid websocket message");
        match message {
            Message::Text(text) => return serde_json::from_str(&text).expect("json"),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected message: {other:?}"),
        }
    }
}

async fn wait_for_records(state: &komari_api::ApiState, uuid: &str, expected: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if records_for(state, uuid).len() >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("record timeout");
}

async fn wait_for_event(
    events: &mut tokio::sync::broadcast::Receiver<komari_api::ClientEvent>,
    kind: ClientEventKind,
) -> komari_api::ClientEvent {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = events.recv().await.expect("event channel");
            if event.kind == kind {
                return event;
            }
        }
    })
    .await
    .expect("event timeout")
}

#[tokio::test]
async fn agent_websocket_reports_and_publishes_presence_events() {
    let (state, app) = app();
    let client = create_client(&state, "ws-node");
    let mut events = state.hub.subscribe();
    let (address, server) = spawn(app).await;

    let url = format!("ws://{address}/api/clients/report?token={}", client.token);
    let (mut socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("agent websocket handshake");

    let online = wait_for_event(&mut events, ClientEventKind::Online).await;
    assert_eq!(online.uuid, client.uuid);
    assert_eq!(online.source, PresenceSource::WebSocket);
    assert!(state.hub.is_online(&client.uuid));

    let body = report_body(&client.uuid);
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .expect("send report");
    wait_for_records(&state, &client.uuid, 1).await;

    let cached = state
        .hub
        .latest_report(&client.uuid)
        .expect("latest report");
    assert!((cached.cpu.usage - 12.5).abs() < f64::EPSILON);

    socket
        .send(Message::Text(
            serde_json::json!({"type": "unknown", "uuid": client.uuid})
                .to_string()
                .into(),
        ))
        .await
        .expect("send unknown frame");
    // Unknown frames are ignored; the connection stays healthy.
    socket
        .send(Message::Text(
            serde_json::json!({"type": "report", "cpu": {"usage": 2.0}})
                .to_string()
                .into(),
        ))
        .await
        .expect("send typed report");
    wait_for_records(&state, &client.uuid, 2).await;

    socket.close(None).await.expect("close socket");
    let offline = wait_for_event(&mut events, ClientEventKind::Offline).await;
    assert_eq!(offline.uuid, client.uuid);
    assert!(!state.hub.is_online(&client.uuid));

    server.abort();
}

#[tokio::test]
async fn agent_websocket_requires_a_valid_token() {
    let (_state, app) = app();
    let (address, server) = spawn(app).await;

    let url = format!("ws://{address}/api/clients/report?token=missing");
    let error = tokio_tungstenite::connect_async(url)
        .await
        .expect_err("handshake must fail");
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 401);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    server.abort();
}

#[tokio::test]
async fn browser_websocket_serves_snapshots_and_rejects_foreign_origins() {
    let (state, app) = app();
    let client = create_client(&state, "web-node");
    state.presence.refresh(&client.uuid);
    state
        .hub
        .set_latest_report(&client.uuid, komari_api::Report::default());
    let (address, server) = spawn(app).await;

    let mut request = format!("ws://{address}/api/clients")
        .into_client_request()
        .expect("request");
    request.headers_mut().insert(
        ORIGIN,
        HeaderValue::from_str(&format!("http://{address}")).expect("origin"),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("same-origin handshake");

    socket
        .send(Message::Text("get".into()))
        .await
        .expect("send get");
    let snapshot = next_text(&mut socket).await;
    assert_eq!(snapshot["status"], "success");
    assert!(
        snapshot["data"]["online"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == &Value::String(client.uuid.clone()))
    );
    assert!(snapshot["data"]["data"][&client.uuid].is_object());

    socket
        .send(Message::Text("get missing".into()))
        .await
        .expect("send filtered get");
    let filtered = next_text(&mut socket).await;
    assert!(filtered["data"]["online"].as_array().unwrap().is_empty());

    socket
        .send(Message::Text("nonsense".into()))
        .await
        .expect("send invalid");
    let invalid = next_text(&mut socket).await;
    assert_eq!(invalid["status"], "error");
    assert_eq!(invalid["error"], "Invalid message");

    socket.close(None).await.expect("close");
    server.abort();

    // A foreign Origin is rejected during the HTTP upgrade.
    let (_state, app) = common::app();
    let (address, server) = spawn(app).await;
    let mut request = format!("ws://{address}/api/clients")
        .into_client_request()
        .expect("request");
    request
        .headers_mut()
        .insert(ORIGIN, HeaderValue::from_static("http://evil.example"));
    let error = tokio_tungstenite::connect_async(request)
        .await
        .expect_err("foreign origin must fail");
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 403);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    server.abort();
}

#[tokio::test]
async fn browser_websocket_allows_cross_origin_when_configured() {
    let (state, app) = app();
    state
        .db
        .config()
        .set(komari_config::keys::ALLOW_CORS_KEY, &true)
        .expect("enable cors");
    let (address, server) = spawn(app).await;

    let mut request = format!("ws://{address}/api/clients")
        .into_client_request()
        .expect("request");
    request
        .headers_mut()
        .insert(ORIGIN, HeaderValue::from_static("http://dashboard.example"));
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("cross-origin handshake");
    socket
        .send(Message::Text("get".into()))
        .await
        .expect("send get");
    assert_eq!(next_text(&mut socket).await["status"], "success");

    socket.close(None).await.expect("close");
    server.abort();
}

#[tokio::test]
async fn browser_websocket_rejects_missing_origin_unless_disabled() {
    let config = ApiConfig {
        ws_disable_origin: true,
        ..default_config()
    };
    let (_state, app) = app_with(config);
    let (address, server) = spawn(app).await;

    let request = format!("ws://{address}/api/clients")
        .into_client_request()
        .expect("request");
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("origin check disabled");
    socket.close(None).await.expect("close");
    server.abort();
}

#[tokio::test]
async fn plain_http_clients_route_is_not_a_websocket_upgrade() {
    let (_state, app) = app();
    let (status, _) = call(
        &app,
        common::request(
            axum::http::Method::GET,
            "/api/clients",
            axum::body::Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
}
