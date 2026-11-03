mod common;

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::{call, create_client, request, rpc_app, set_api_key};
use futures_util::{SinkExt, StreamExt};
use komari_api::Report;
use komari_db::models::Record;
use komari_db::{ClientPatch, LocalTime};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

const API_KEY: &str = "0123456789abcdef";

fn post(body: Value) -> Request<Body> {
    request(Method::POST, "/api/rpc2", Body::from(body.to_string()))
}

fn post_bearer(body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/api/rpc2")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {API_KEY}"))
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn post_query(body: Value, token: &str) -> Request<Body> {
    request(
        Method::POST,
        &format!("/api/rpc2?Authorization={token}"),
        Body::from(body.to_string()),
    )
}

fn rpc(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

async fn spawn(app: Router) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (address, handle)
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(address: SocketAddr) -> Socket {
    tokio_tungstenite::connect_async(format!("ws://{address}/api/rpc2"))
        .await
        .unwrap()
        .0
}

async fn next_json(socket: &mut Socket) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .expect("websocket timeout")
            .expect("websocket message")
            .expect("valid message");
        match message {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected message: {other:?}"),
        }
    }
}

#[tokio::test]
async fn rpc2_post_public_info_for_guest() {
    let (_state, app) = rpc_app();
    let (status, body) = call(&app, post(rpc(1, "common:getPublicInfo", json!({})))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 1);
    assert_eq!(body["result"]["sitename"], "Komari");
    assert_eq!(body["result"]["theme"], "default");
    assert_eq!(body["result"]["record_enabled"], true);
}

#[tokio::test]
async fn rpc2_get_nodes_filters_hidden_and_sensitive_fields() {
    let (state, app) = rpc_app();
    let visible = create_client(&state, "visible");
    let hidden = create_client(&state, "hidden");
    state
        .db
        .clients()
        .patch(
            &visible.uuid,
            &ClientPatch {
                ipv4: Some("203.0.113.10".to_owned()),
                ipv6: Some("2001:db8::1".to_owned()),
                remark: Some("secret".to_owned()),
                version: Some("1.0".to_owned()),
                ..ClientPatch::default()
            },
        )
        .unwrap();
    state
        .db
        .clients()
        .patch(
            &hidden.uuid,
            &ClientPatch {
                hidden: Some(true),
                ..ClientPatch::default()
            },
        )
        .unwrap();
    state
        .db
        .config()
        .set("send_ip_addr_to_guest", &true)
        .unwrap();

    let (status, body) = call(&app, post(rpc(1, "common:getNodes", json!({})))).await;
    assert_eq!(status, StatusCode::OK);
    let nodes = body["result"].as_object().unwrap();
    assert!(nodes.contains_key(&visible.uuid));
    assert!(!nodes.contains_key(&hidden.uuid));
    let node = &nodes[&visible.uuid];
    assert!(node.get("token").is_none());
    assert!(node.get("remark").is_none());
    assert!(node.get("version").is_none());
    assert_eq!(node["ipv4"], "203.*.*.*");
    assert_eq!(node["ipv6"], "2001:*:*:*:*:*:*:*");

    // Admin sees the complete record.
    set_api_key(&state, API_KEY);
    let (status, body) = call(
        &app,
        post_bearer(rpc(2, "common:getNodes", json!({"uuid": visible.uuid}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["token"], visible.token);
    assert_eq!(body["result"]["remark"], "secret");
}

#[tokio::test]
async fn rpc2_namespace_permissions() {
    let (state, app) = rpc_app();
    let client = create_client(&state, "agent");

    let (status, body) = call(&app, post(rpc(1, "admin:getClients", json!({})))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], 401);
    assert_eq!(body["error"]["message"], "Unauthorized");

    let (status, body) = call(
        &app,
        post_query(rpc(2, "admin:getClients", json!({})), &client.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], 401);

    let (status, body) = call(
        &app,
        post_query(rpc(3, "common:getPublicInfo", json!({})), &client.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["result"].is_object());

    let (status, body) = call(&app, post(rpc(4, "unknown:method", json!({})))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], 401);
}

#[tokio::test]
async fn rpc2_admin_client_and_settings_methods() {
    let (state, app) = rpc_app();
    set_api_key(&state, API_KEY);
    let client = create_client(&state, "alpha");

    let (status, body) = call(&app, post_bearer(rpc(1, "admin:getClients", json!({})))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"].as_array().unwrap().len(), 1);
    assert_eq!(body["result"][0]["name"], "alpha");

    let (status, body) = call(
        &app,
        post_bearer(rpc(2, "admin:getClient", json!({"uuid": client.uuid}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["uuid"], client.uuid);

    let (status, body) = call(
        &app,
        post_bearer(rpc(3, "admin:getClient", json!({"uuid": "missing"}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], -32044);

    let (status, body) = call(
        &app,
        post_bearer(rpc(4, "admin:saveSettings", json!({"sitename": "Rpc"}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("result").is_none());
    assert!(body.get("error").is_none());

    let (status, body) = call(&app, post_bearer(rpc(5, "admin:getSettings", json!({})))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["sitename"], "Rpc");

    let (status, body) = call(
        &app,
        post_bearer(rpc(6, "admin:addClient", json!({"name": "beta"}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let beta = body["result"]["uuid"].as_str().unwrap().to_owned();
    let (_, body) = call(
        &app,
        post_bearer(rpc(
            7,
            "admin:editClient",
            json!({"uuid": beta, "hidden": true, "weight": 3}),
        )),
    )
    .await;
    assert!(body.get("error").is_none());
    let (_, body) = call(
        &app,
        post_bearer(rpc(8, "admin:getClient", json!({"uuid": beta}))),
    )
    .await;
    assert_eq!(body["result"]["hidden"], true);
    assert_eq!(body["result"]["weight"], 3);

    let (_, body) = call(
        &app,
        post_bearer(rpc(9, "admin:removeClient", json!({"uuid": beta}))),
    )
    .await;
    assert!(body.get("error").is_none());
    assert_eq!(state.db.clients().list().unwrap().len(), 1);
}

#[tokio::test]
async fn rpc2_errors_match_go_codes() {
    let (_state, app) = rpc_app();

    let (status, body) = call(&app, post(rpc(1, "common:doesNotExist", json!({})))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], -32601);
    assert_eq!(body["error"]["message"], "method not found");
    assert_eq!(body["error"]["data"], "common:doesNotExist");

    let (status, body) = call(
        &app,
        request(Method::POST, "/api/rpc2", Body::from("{not json")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32700);

    let (status, body) = call(
        &app,
        post(json!({"jsonrpc": "1.0", "id": 2, "method": "rpc.ping"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32600);
    assert_eq!(body["error"]["message"], "invalid jsonrpc version");

    let (status, body) = call(&app, request(Method::POST, "/api/rpc2", Body::from("[]"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32600);
    assert_eq!(body["error"]["message"], "empty batch");
}

#[tokio::test]
async fn rpc2_batch_and_internal_methods() {
    let (_state, app) = rpc_app();
    let batch = json!([
        {"jsonrpc": "2.0", "id": 1, "method": "rpc.ping"},
        {"jsonrpc": "2.0", "id": 2, "method": "rpc.version"},
    ]);
    let (status, body) = call(&app, post(batch)).await;
    assert_eq!(status, StatusCode::OK);
    let responses = body.as_array().unwrap();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["result"], "pong");
    assert_eq!(responses[1]["result"], "2.0");

    let (_, body) = call(&app, post(rpc(3, "rpc.methods", json!({})))).await;
    let methods = body["result"].as_array().unwrap();
    assert!(methods.iter().any(|method| method == "common:getNodes"));
    assert!(!methods.iter().any(|method| method == "rpc.ping"));

    let (_, body) = call(&app, post(rpc(4, "rpc.methods", json!({"internal": true})))).await;
    let methods = body["result"].as_array().unwrap();
    assert!(methods.iter().any(|method| method == "rpc.ping"));

    let (_, body) = call(
        &app,
        post(rpc(5, "rpc.help", json!({"method": "common:getNodes"}))),
    )
    .await;
    assert_eq!(body["result"]["name"], "common:getNodes");
    assert_eq!(body["result"]["summary"], "Get all nodes");

    let (_, body) = call(&app, post(rpc(6, "rpc.help", json!({"method": "nope"})))).await;
    assert_eq!(body["error"]["code"], -32600);
}

#[tokio::test]
async fn rpc2_get_me_for_each_permission_group() {
    let (state, app) = rpc_app();
    let client = create_client(&state, "agent");
    let session = common::create_admin_session(&state);

    let (_, body) = call(&app, post(rpc(1, "common:getMe", json!({})))).await;
    assert_eq!(body["result"]["logged_in"], false);
    assert_eq!(body["result"]["username"], "");

    let (_, body) = call(
        &app,
        post_query(rpc(2, "common:getMe", json!({})), &client.token),
    )
    .await;
    assert_eq!(body["result"]["logged_in"], true);
    assert_eq!(body["result"]["username"], "client");
    assert_eq!(body["result"]["uuid"], client.uuid);

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/rpc2")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, format!("session_token={session}"))
        .body(Body::from(rpc(3, "common:getMe", json!({})).to_string()))
        .unwrap();
    let (_, body) = call(&app, request).await;
    assert_eq!(body["result"]["logged_in"], true);
    assert_eq!(body["result"]["username"], "admin");
    assert_eq!(body["result"]["uuid"].as_str().unwrap().len(), 36);
}

#[tokio::test]
async fn rpc2_get_records_groups_and_filters_load_type() {
    let (state, app) = rpc_app();
    let client = create_client(&state, "alpha");
    let now = LocalTime::now();
    for offset in 0..3 {
        let mut record = Record::new(client.uuid.clone());
        record.time = now.add_seconds(-offset * 60);
        record.cpu = 10.0 + offset as f32;
        record.ram = 100;
        record.ram_total = 200;
        state.db.records().insert(&record).unwrap();
    }

    let (status, body) = call(
        &app,
        post(rpc(
            1,
            "common:getRecords",
            json!({"type": "load", "hours": 1}),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["count"], 3);
    assert_eq!(
        body["result"]["records"][client.uuid.as_str()]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert!(body["result"]["from"].is_string());
    assert!(body["result"]["to"].is_string());

    let (_, body) = call(
        &app,
        post(rpc(
            2,
            "common:getRecords",
            json!({"type": "load", "uuid": client.uuid, "hours": 1, "load_type": "cpu"}),
        )),
    )
    .await;
    assert_eq!(body["result"]["load_type"], "cpu");
    let entry = &body["result"]["records"][client.uuid.as_str()][0];
    assert!(entry.get("cpu").is_some());
    assert!(entry.get("ram").is_none());

    let (_, body) = call(
        &app,
        post(rpc(3, "common:getRecords", json!({"type": "bad"}))),
    )
    .await;
    assert_eq!(body["error"]["code"], -32602);
}

#[tokio::test]
async fn rpc2_latest_and_recent_status() {
    let (state, app) = rpc_app();
    let client = create_client(&state, "alpha");
    let mut report = Report {
        uuid: client.uuid.clone(),
        ..Report::default()
    };
    report.cpu.usage = 12.5;
    report.ram.used = 10;
    report.ram.total = 20;
    report.mark_updated();
    state.hub.set_latest_report(&client.uuid, report.clone());
    state.report_cache.record(&client.uuid, report);

    let (status, body) = call(
        &app,
        post(rpc(
            1,
            "common:getNodesLatestStatus",
            json!({"uuid": client.uuid}),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["client"], client.uuid);
    assert_eq!(body["result"]["cpu"], 12.5);
    assert_eq!(body["result"]["online"], false);

    let (_, body) = call(
        &app,
        post(rpc(
            2,
            "common:getNodeRecentStatus",
            json!({"uuid": client.uuid}),
        )),
    )
    .await;
    assert_eq!(body["result"]["count"], 1);
    assert_eq!(body["result"]["records"][0]["client"], client.uuid);

    // Hidden clients are rejected for guests.
    state
        .db
        .clients()
        .patch(
            &client.uuid,
            &ClientPatch {
                hidden: Some(true),
                ..ClientPatch::default()
            },
        )
        .unwrap();
    let (_, body) = call(
        &app,
        post(rpc(
            3,
            "common:getNodeRecentStatus",
            json!({"uuid": client.uuid}),
        )),
    )
    .await;
    assert_eq!(body["error"]["code"], -32602);
}

#[tokio::test]
async fn rpc2_websocket_ping_and_permissions() {
    let (state, app) = rpc_app();
    state.db.config().set("allow_cors", &true).unwrap();
    let (address, server) = spawn(app).await;
    let mut socket = connect(address).await;

    socket
        .send(Message::Text(
            rpc(1, "rpc.ping", json!({})).to_string().into(),
        ))
        .await
        .unwrap();
    let response = next_json(&mut socket).await;
    assert_eq!(response["result"], "pong");

    socket
        .send(Message::Text(
            rpc(2, "common:getPublicInfo", json!({})).to_string().into(),
        ))
        .await
        .unwrap();
    let response = next_json(&mut socket).await;
    assert_eq!(response["result"]["sitename"], "Komari");

    socket
        .send(Message::Text(
            rpc(3, "admin:getClients", json!({})).to_string().into(),
        ))
        .await
        .unwrap();
    let response = next_json(&mut socket).await;
    assert_eq!(response["error"]["code"], 401);

    // Malformed frames produce an InvalidRequest response and keep the socket.
    socket.send(Message::Text("not json".into())).await.unwrap();
    let response = next_json(&mut socket).await;
    assert_eq!(response["error"]["code"], -32600);

    socket.close(None).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn rpc2_websocket_rejects_origin_without_cors() {
    let (_state, app) = rpc_app();
    let (address, server) = spawn(app).await;
    let result = tokio_tungstenite::connect_async(format!("ws://{address}/api/rpc2")).await;
    assert!(result.is_err());
    server.abort();
}
