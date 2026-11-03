mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use common::{
    app, app_with, app_with_clock, call, create_client, default_config, flush, records_for,
    report_body,
};
use futures_util::{SinkExt, StreamExt};
use komari_api::{ApiConfig, ClientEventKind, ManualClock, PresenceSource};
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

async fn connect_agent(address: SocketAddr, token: &str) -> Socket {
    let url = format!("ws://{address}/api/clients/report?token={token}");
    tokio_tungstenite::connect_async(url)
        .await
        .expect("agent websocket handshake")
        .0
}

#[tokio::test]
async fn agent_websocket_validates_first_frame_before_going_online() {
    let (state, app) = app();
    let client = create_client(&state, "ws-node");
    let mut events = state.hub.subscribe();
    let (address, server) = spawn(app).await;
    let mut socket = connect_agent(address, &client.token).await;

    // No online event before the first valid frame.
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert!(!state.hub.is_online(&client.uuid));

    socket
        .send(Message::Text(report_body(&client.uuid).to_string().into()))
        .await
        .expect("send first report");

    let online = wait_for_event(&mut events, ClientEventKind::Online).await;
    assert_eq!(online.uuid, client.uuid);
    assert_eq!(online.source, PresenceSource::WebSocket);
    assert!(state.hub.is_online(&client.uuid));
    assert_eq!(state.report_cache.buffered(&client.uuid), 1);

    socket
        .send(Message::Text(
            serde_json::json!({"type": "report", "cpu": {"usage": 2.0}})
                .to_string()
                .into(),
        ))
        .await
        .expect("send typed report");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if state.report_cache.buffered(&client.uuid) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second frame timeout");

    flush(&state);
    assert_eq!(records_for(&state, &client.uuid).len(), 1);

    socket.close(None).await.expect("close socket");
    let offline = wait_for_event(&mut events, ClientEventKind::Offline).await;
    assert_eq!(offline.uuid, client.uuid);
    assert!(!state.hub.is_online(&client.uuid));

    server.abort();
}

#[tokio::test]
async fn agent_websocket_invalid_first_frame_closes_without_online() {
    let (state, app) = app();
    let client = create_client(&state, "ws-node");
    let mut events = state.hub.subscribe();
    let (address, server) = spawn(app).await;
    let mut socket = connect_agent(address, &client.token).await;

    socket
        .send(Message::Text("{not json".into()))
        .await
        .expect("send invalid frame");
    let closed = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("close timeout");
    assert!(closed.is_none() || matches!(closed, Some(Ok(Message::Close(_))) | Some(Err(_))));

    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert!(!state.hub.is_online(&client.uuid));
    assert_eq!(state.report_cache.buffered(&client.uuid), 0);

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
async fn agent_websocket_read_timeout_marks_client_offline() {
    let config = ApiConfig {
        socket_read_timeout: Duration::from_millis(50),
        ..default_config()
    };
    let (state, app) = app_with(config);
    let client = create_client(&state, "timeout-node");
    let mut events = state.hub.subscribe();
    let (address, server) = spawn(app).await;
    let mut socket = connect_agent(address, &client.token).await;
    socket
        .send(Message::Text(report_body(&client.uuid).to_string().into()))
        .await
        .expect("send first report");
    let _ = wait_for_event(&mut events, ClientEventKind::Online).await;

    let offline = wait_for_event(&mut events, ClientEventKind::Offline).await;
    assert_eq!(offline.uuid, client.uuid);
    assert!(!state.hub.is_online(&client.uuid));

    server.abort();
}

#[tokio::test]
async fn reconnect_closes_the_previous_websocket() {
    let (state, app) = app();
    let client = create_client(&state, "ws-node");
    let mut events = state.hub.subscribe();
    let (address, server) = spawn(app).await;

    let mut first = connect_agent(address, &client.token).await;
    first
        .send(Message::Text(report_body(&client.uuid).to_string().into()))
        .await
        .expect("first report");
    let _ = wait_for_event(&mut events, ClientEventKind::Online).await;
    let first_id = state.hub.connection_id(&client.uuid).unwrap();

    let mut second = connect_agent(address, &client.token).await;
    second
        .send(Message::Text(report_body(&client.uuid).to_string().into()))
        .await
        .expect("second report");
    let _ = wait_for_event(&mut events, ClientEventKind::Online).await;

    let closed = tokio::time::timeout(Duration::from_secs(2), first.next())
        .await
        .expect("old socket close timeout");
    assert!(closed.is_none() || matches!(closed, Some(Ok(Message::Close(_))) | Some(Err(_))));
    assert_eq!(state.hub.connected_count(), 1);
    assert_ne!(state.hub.connection_id(&client.uuid).unwrap(), first_id);

    second.close(None).await.expect("close second");
    server.abort();
}

#[tokio::test]
async fn ws_and_http_presence_are_a_union_for_offline_events() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let (state, app) = app_with_clock(default_config(), clock.clone());
    let client = create_client(&state, "union-node");
    let mut events = state.hub.subscribe();
    let (address, server) = spawn(app).await;

    let mut socket = connect_agent(address, &client.token).await;
    socket
        .send(Message::Text(report_body(&client.uuid).to_string().into()))
        .await
        .expect("first report");
    let _ = wait_for_event(&mut events, ClientEventKind::Online).await;

    // HTTP POST presence also exists.
    state.presence.refresh(&client.uuid);
    let _ = events.try_recv(); // HttpPost online event

    // Closing the WebSocket must not emit Offline while POST presence lives.
    socket.close(None).await.expect("close socket");
    tokio::time::timeout(Duration::from_millis(200), async {
        while let Ok(event) = events.try_recv() {
            assert_ne!(event.kind, ClientEventKind::Offline);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    })
    .await
    .expect("unexpected offline event");
    assert!(state.hub.is_online(&client.uuid));

    // Expiring POST presence without a WS now emits the offline event.
    clock.advance(11_000);
    state.presence.expire_due();
    let offline = wait_for_event(&mut events, ClientEventKind::Offline).await;
    assert_eq!(offline.source, PresenceSource::HttpPost);

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
            let body = response.body().as_deref().unwrap_or_default();
            assert_eq!(
                String::from_utf8_lossy(body),
                "request origin not allowed by Upgrader.CheckOrigin"
            );
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
async fn allow_cors_runtime_toggle_is_honoured() {
    let (state, app) = app();
    state
        .db
        .config()
        .set(komari_config::keys::ALLOW_CORS_KEY, &true)
        .expect("enable cors");
    let (address, server) = spawn(app).await;
    let request = || {
        let mut request = format!("ws://{address}/api/clients")
            .into_client_request()
            .expect("request");
        request
            .headers_mut()
            .insert(ORIGIN, HeaderValue::from_static("http://dashboard.example"));
        request
    };

    let (mut socket, _) = tokio_tungstenite::connect_async(request())
        .await
        .expect("cors enabled");
    socket.close(None).await.expect("close");

    state
        .db
        .config()
        .set(komari_config::keys::ALLOW_CORS_KEY, &false)
        .expect("disable cors");
    let error = tokio_tungstenite::connect_async(request())
        .await
        .expect_err("cors disabled");
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 403);
        }
        other => panic!("unexpected error: {other:?}"),
    }

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
async fn plain_http_clients_route_returns_go_error_body() {
    let (_state, app) = app();
    let (status, body) = call(
        &app,
        common::request(
            axum::http::Method::GET,
            "/api/clients",
            axum::body::Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        serde_json::json!({"status": "error", "error": "Require WebSocket upgrade"})
    );
}

#[tokio::test]
async fn http_expiry_does_not_emit_offline_while_ws_is_live() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let (state, app) = app_with_clock(default_config(), clock.clone());
    let client = create_client(&state, "union-node");
    let mut events = state.hub.subscribe();
    let (address, server) = spawn(app).await;

    let mut socket = connect_agent(address, &client.token).await;
    socket
        .send(Message::Text(report_body(&client.uuid).to_string().into()))
        .await
        .expect("first report");
    let _ = wait_for_event(&mut events, ClientEventKind::Online).await;

    state.presence.refresh(&client.uuid);
    let _ = wait_for_event(&mut events, ClientEventKind::Online).await;

    clock.advance(11_000);
    state.presence.expire_due();
    tokio::time::timeout(Duration::from_millis(200), async {
        while let Ok(event) = events.try_recv() {
            assert_ne!(event.kind, ClientEventKind::Offline);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    })
    .await
    .expect("unexpected offline event");
    assert!(state.hub.is_online(&client.uuid));

    socket.close(None).await.expect("close socket");
    let offline = wait_for_event(&mut events, ClientEventKind::Offline).await;
    assert_eq!(offline.source, PresenceSource::WebSocket);

    server.abort();
}
