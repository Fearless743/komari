//! HTTP server assembly tests: routing, static fallback, browser WebSocket,
//! OAuth, background tasks, backup restore and graceful shutdown.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use komari_api::server::{
    BackgroundConfig, OAuthRuntime, Server, ServerBuilder, ServerConfig, TEMP_SHARE_EXPIRE_KEY,
    TEMP_SHARE_TOKEN_KEY, refresh_web_assets,
};
use komari_api::{ApiState, Report};
use komari_db::{BackupOptions, ClientPatch, Db};
use komari_oauth::{
    AuthorizationRequest, CallbackRequest, ExchangeRequest, OAuthError, OAuthProvider, OAuthUser,
    StateStore, TokenSet, UserInfo,
};
use komari_web::WebAssets;
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::{COOKIE, ORIGIN};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Running {
    base: String,
    address: String,
    db: Arc<Db>,
    assets: Arc<WebAssets>,
    shutdown: oneshot::Sender<()>,
    handle: JoinHandle<anyhow::Result<()>>,
}

impl Running {
    async fn stop(self) {
        let _ = self.shutdown.send(());
        let _ = tokio::time::timeout(Duration::from_secs(5), self.handle).await;
    }
}

fn server_config(dir: &TempDir) -> ServerConfig {
    ServerConfig {
        listen: "127.0.0.1:0".to_owned(),
        database_path: dir.path().join("komari.db"),
        data_dir: dir.path().to_path_buf(),
        background: BackgroundConfig {
            enabled: false,
            ..BackgroundConfig::default()
        },
        request_logging: false,
        ..ServerConfig::default()
    }
}

fn memory_db() -> Db {
    let db = Db::open_in_memory().expect("open in-memory database");
    db.migrate().expect("migrate database");
    db
}

async fn build(config: ServerConfig, db: Db) -> Server {
    ServerBuilder::new(config)
        .with_db(db)
        .skip_services()
        .build()
        .await
        .expect("build server")
}

async fn start(server: Server) -> Running {
    let db = server.state().db.clone();
    let assets = server.assets().clone();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind random port");
    let address = listener.local_addr().expect("address").to_string();
    let base = format!("http://{address}");
    let (shutdown, receiver) = oneshot::channel();
    let handle = tokio::spawn(server.serve_with_shutdown(listener, async {
        let _ = receiver.await;
    }));
    Running {
        base,
        address,
        db,
        assets,
        shutdown,
        handle,
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("http client")
}

async fn get_json(client: &reqwest::Client, url: &str) -> (reqwest::StatusCode, Value) {
    let response = client.get(url).send().await.expect("request");
    let status = response.status();
    let value = response.json().await.unwrap_or(Value::Null);
    (status, value)
}

async fn browser_socket(address: &str, cookie: Option<&str>) -> Socket {
    let mut request = format!("ws://{address}/api/clients")
        .into_client_request()
        .expect("websocket request");
    request.headers_mut().insert(
        ORIGIN,
        HeaderValue::from_str(&format!("http://{address}")).expect("origin"),
    );
    if let Some(cookie) = cookie {
        request
            .headers_mut()
            .insert(COOKIE, HeaderValue::from_str(cookie).expect("cookie"));
    }
    tokio_tungstenite::connect_async(request)
        .await
        .expect("browser websocket handshake")
        .0
}

async fn next_json(socket: &mut Socket) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .expect("websocket timeout")
        .expect("websocket message")
        .expect("valid message");
    let text = message.into_text().expect("text frame");
    serde_json::from_str(text.as_str()).expect("json frame")
}

fn create_admin_session(db: &Db) -> String {
    let user = db
        .accounts()
        .create("admin", "password")
        .expect("create admin");
    komari_auth::create_session(db, &user.uuid, "test", "127.0.0.1", "password")
        .expect("create session")
}

#[tokio::test]
async fn server_starts_and_serves_ping() {
    let dir = TempDir::new().expect("temp dir");
    let config = server_config(&dir);
    let server = Server::start(config).await.expect("start server");
    let running = start(server).await;
    let client = http_client();

    let response = client
        .get(format!("{}/ping", running.base))
        .send()
        .await
        .expect("ping");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.text().await.expect("body"), "pong");

    running.stop().await;
}

#[tokio::test]
async fn server_initializes_the_geoip_service() {
    let dir = TempDir::new().expect("temp dir");
    let server = Server::start(server_config(&dir))
        .await
        .expect("start server");
    let geoip = server.state().geoip.as_ref().expect("geoip service");
    assert_ne!(geoip.provider_key(), "empty");
    assert_eq!(geoip.provider_key(), "ipinfo");
}

#[tokio::test]
async fn server_installs_web_and_ddns_services_in_state() {
    let dir = TempDir::new().expect("temp dir");
    let hook_ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&hook_ran);
    let server = ServerBuilder::new(server_config(&dir))
        .with_db(memory_db())
        .skip_services()
        .configure_state(move |state| {
            flag.store(true, Ordering::SeqCst);
            state
        })
        .build()
        .await
        .expect("build");
    assert!(hook_ran.load(Ordering::SeqCst));
    assert_eq!(
        server
            .state()
            .web
            .as_ref()
            .expect("web assets")
            .config()
            .data_dir
            .as_path(),
        dir.path()
    );
    assert!(server.state().ddns.is_some());
}

#[tokio::test]
async fn server_mounts_client_routes_and_static_fallback() {
    let dir = TempDir::new().expect("temp dir");
    let server = build(server_config(&dir), memory_db()).await;
    let running = start(server).await;
    let client = http_client();

    let (status, body) = get_json(&client, &format!("{}/api/clients", running.base)).await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "Require WebSocket upgrade");

    let response = client
        .get(format!("{}/dashboard", running.base))
        .send()
        .await
        .expect("spa fallback");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.starts_with("text/html"), "{content_type}");
    let body = response.text().await.expect("html");
    assert!(body.contains("Komari default theme placeholder"), "{body}");

    let response = client
        .get(format!("{}/api/does-not-exist", running.base))
        .send()
        .await
        .expect("api 404");
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    assert!(
        !response
            .text()
            .await
            .expect("404 body")
            .contains("Komari default theme")
    );

    running.stop().await;
}

#[tokio::test]
async fn static_admin_forces_default_theme() {
    let dir = TempDir::new().expect("temp dir");
    let custom = dir.path().join("theme/custom/dist");
    std::fs::create_dir_all(&custom).expect("theme dir");
    std::fs::write(
        custom.join("index.html"),
        "<html><body>CUSTOM-THEME-MARKER</body></html>",
    )
    .expect("custom index");

    let db = memory_db();
    db.config().set("theme", &"custom").expect("set theme");
    let server = build(server_config(&dir), db).await;
    let running = start(server).await;
    let client = http_client();

    let body = client
        .get(format!("{}/dashboard", running.base))
        .send()
        .await
        .expect("dashboard")
        .text()
        .await
        .expect("dashboard body");
    assert!(body.contains("CUSTOM-THEME-MARKER"), "{body}");

    let admin = client
        .get(format!("{}/admin/settings", running.base))
        .send()
        .await
        .expect("admin")
        .text()
        .await
        .expect("admin body");
    assert!(!admin.contains("CUSTOM-THEME-MARKER"), "{admin}");
    assert!(
        admin.contains("Komari default theme placeholder"),
        "{admin}"
    );

    running.stop().await;
}

#[tokio::test]
async fn site_name_changes_are_visible_immediately() {
    let dir = TempDir::new().expect("temp dir");
    let db = memory_db();
    let server = build(server_config(&dir), db).await;
    let running = start(server).await;
    let client = http_client();

    let before = client
        .get(format!("{}/dashboard", running.base))
        .send()
        .await
        .expect("dashboard")
        .text()
        .await
        .expect("body");
    assert!(before.contains("<title>Komari Monitor</title>"), "{before}");

    running
        .db
        .config()
        .set("sitename", &"Immediate Site")
        .expect("set sitename");
    let after = client
        .get(format!("{}/dashboard", running.base))
        .send()
        .await
        .expect("dashboard")
        .text()
        .await
        .expect("body");
    assert!(after.contains("<title>Immediate Site</title>"), "{after}");

    running.stop().await;
}

#[tokio::test]
async fn temp_share_token_is_loaded_from_the_database() {
    let dir = TempDir::new().expect("temp dir");
    let db = memory_db();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    db.config()
        .set(TEMP_SHARE_TOKEN_KEY, &"share-token")
        .expect("token");
    db.config()
        .set(TEMP_SHARE_EXPIRE_KEY, &(now + 120))
        .expect("expiry");
    let server = build(server_config(&dir), db).await;
    let running = start(server).await;
    let client = http_client();

    let response = client
        .get(format!("{}/dashboard?temp_key=share-token", running.base))
        .send()
        .await
        .expect("share link");
    let cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(cookie.contains("temp_key=share-token"), "{cookie}");

    // Expire the token and refresh the in-memory asset snapshot.
    running
        .db
        .config()
        .set(TEMP_SHARE_EXPIRE_KEY, &(now - 1))
        .expect("expire token");
    refresh_web_assets(&running.assets, &running.db).expect("refresh assets");
    let response = client
        .get(format!("{}/dashboard?temp_key=share-token", running.base))
        .send()
        .await
        .expect("expired share link");
    assert!(
        response.headers().get("set-cookie").is_none(),
        "expired token must not set a cookie"
    );

    running.stop().await;
}

#[tokio::test]
async fn browser_websocket_filters_hidden_clients_for_guests() {
    let dir = TempDir::new().expect("temp dir");
    let db = memory_db();
    let visible = db.clients().create_named("visible").expect("visible");
    let hidden = db.clients().create_named("hidden").expect("hidden");
    db.clients()
        .patch(
            &hidden.uuid,
            &ClientPatch {
                hidden: Some(true),
                ..ClientPatch::default()
            },
        )
        .expect("hide client");
    let server = build(server_config(&dir), db).await;
    let state = server.state().clone();
    for client in [&visible, &hidden] {
        state.presence.refresh(&client.uuid);
        state.hub.set_latest_report(&client.uuid, Report::default());
    }
    let running = start(server).await;

    let mut guest = browser_socket(&running.address, None).await;
    guest
        .send(Message::Text("get".into()))
        .await
        .expect("send get");
    let snapshot = next_json(&mut guest).await;
    let online = snapshot["data"]["online"].as_array().expect("online");
    assert!(
        online
            .iter()
            .any(|uuid| uuid.as_str() == Some(visible.uuid.as_str()))
    );
    assert!(
        !online
            .iter()
            .any(|uuid| uuid.as_str() == Some(hidden.uuid.as_str()))
    );
    assert!(snapshot["data"]["data"][visible.uuid.as_str()].is_object());
    assert!(snapshot["data"]["data"][hidden.uuid.as_str()].is_null());
    guest.close(None).await.expect("close");

    let session = create_admin_session(&state.db);
    let cookie = format!("session_token={session}");
    let mut admin = browser_socket(&running.address, Some(&cookie)).await;
    admin
        .send(Message::Text("get".into()))
        .await
        .expect("send get");
    let snapshot = next_json(&mut admin).await;
    let online = snapshot["data"]["online"].as_array().expect("online");
    assert!(
        online
            .iter()
            .any(|uuid| uuid.as_str() == Some(visible.uuid.as_str()))
    );
    assert!(
        online
            .iter()
            .any(|uuid| uuid.as_str() == Some(hidden.uuid.as_str()))
    );
    assert!(snapshot["data"]["data"][hidden.uuid.as_str()].is_object());
    admin.close(None).await.expect("close");

    running.stop().await;
}

#[tokio::test]
async fn browser_websocket_rejects_guests_in_private_site_mode() {
    let dir = TempDir::new().expect("temp dir");
    let db = memory_db();
    db.config()
        .set("private_site", &true)
        .expect("private site");
    let server = build(server_config(&dir), db).await;
    let state = server.state().clone();
    let running = start(server).await;

    let mut request = format!("ws://{}/api/clients", running.address)
        .into_client_request()
        .expect("request");
    request.headers_mut().insert(
        ORIGIN,
        HeaderValue::from_str(&format!("http://{}", running.address)).expect("origin"),
    );
    let error = tokio_tungstenite::connect_async(request)
        .await
        .expect_err("guest must be rejected");
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 401);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let session = create_admin_session(&state.db);
    let cookie = format!("session_token={session}");
    let mut socket = browser_socket(&running.address, Some(&cookie)).await;
    socket
        .send(Message::Text("get".into()))
        .await
        .expect("send get");
    assert_eq!(next_json(&mut socket).await["status"], "success");
    socket.close(None).await.expect("close");

    running.stop().await;
}

#[tokio::test]
async fn request_body_limit_is_enforced_before_auth() {
    let dir = TempDir::new().expect("temp dir");
    let db = memory_db();
    db.config().set("allow_cors", &true).expect("allow cors");
    let config = ServerConfig {
        body_limit_bytes: 16,
        ..server_config(&dir)
    };
    let server = build(config, db).await;
    let running = start(server).await;
    let client = http_client();

    let response = client
        .post(format!("{}/api/clients/register", running.base))
        .body("x".repeat(1024))
        .send()
        .await
        .expect("oversized request");
    assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("*")
    );

    running.stop().await;
}

#[tokio::test]
async fn chunked_oversized_body_returns_413_with_cors() {
    let dir = TempDir::new().expect("temp dir");
    let db = memory_db();
    db.config().set("allow_cors", &true).expect("allow cors");
    let config = ServerConfig {
        body_limit_bytes: 16,
        ..server_config(&dir)
    };
    let server = build(config, db).await;
    let running = start(server).await;

    let request = concat!(
        "POST /api/clients/register HTTP/1.1\r\n",
        "Host: localhost\r\n",
        "Transfer-Encoding: chunked\r\n",
        "Content-Type: application/json\r\n",
        "Origin: https://dashboard.example\r\n",
        "Connection: close\r\n\r\n",
        "10\r\n0123456789abcdef\r\n",
        "10\r\n0123456789abcdef\r\n",
        "0\r\n\r\n"
    );
    let mut stream = TcpStream::connect(&running.address).await.expect("connect");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write chunked request");
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut response))
        .await
        .expect("response timeout")
        .expect("read response");
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
    assert!(
        response
            .to_ascii_lowercase()
            .contains("access-control-allow-origin: *"),
        "{response}"
    );

    running.stop().await;
}

#[tokio::test]
async fn backup_upload_path_has_a_larger_body_limit() {
    let dir = TempDir::new().expect("temp dir");
    let config = ServerConfig {
        body_limit_bytes: 16 * 1024 * 1024,
        backup_body_limit_bytes: 32 * 1024 * 1024,
        ..server_config(&dir)
    };
    let server = build(config, memory_db()).await;
    let running = start(server).await;
    let client = http_client();
    let body = "x".repeat(17 * 1024 * 1024);

    let response = client
        .post(format!("{}/api/clients/register", running.base))
        .body(body.clone())
        .send()
        .await
        .expect("normal oversized request");
    assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);

    // The backup path is not implemented by B09, so the request may end in
    // 401/404, but it must not be rejected by the normal 16 MiB limit.
    let response = client
        .post(format!("{}/api/admin/upload/backup", running.base))
        .body(body)
        .send()
        .await
        .expect("backup request");
    assert_ne!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);

    running.stop().await;
}

#[tokio::test]
async fn cors_headers_and_options_are_dynamic() {
    let dir = TempDir::new().expect("temp dir");
    let db = memory_db();
    db.config().set("allow_cors", &true).expect("allow cors");
    let server = build(server_config(&dir), db).await;
    let running = start(server).await;
    let client = http_client();

    let response = client
        .get(format!("{}/ping", running.base))
        .header("Origin", "https://dashboard.example")
        .send()
        .await
        .expect("cors get");
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("*")
    );

    let response = client
        .request(reqwest::Method::OPTIONS, format!("{}/ping", running.base))
        .send()
        .await
        .expect("preflight");
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-methods")
            .and_then(|value| value.to_str().ok()),
        Some("GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS")
    );

    running.stop().await;
}

#[tokio::test]
async fn b04_b06_routers_can_be_merged() {
    let dir = TempDir::new().expect("temp dir");
    let public = Router::<ApiState>::new().route("/api/public/test", get(|| async { "public" }));
    let admin = Router::<ApiState>::new().route("/api/admin/test", get(|| async { "admin" }));
    let rpc = Router::<ApiState>::new().route("/api/rpc2", get(|| async { "rpc" }));
    let stateless = Router::new().route("/stateless", get(|| async { "stateless" }));

    let server = ServerBuilder::new(server_config(&dir))
        .with_db(memory_db())
        .skip_services()
        .merge_public(public)
        .merge_admin(admin)
        .merge_rpc(rpc)
        .merge_stateless(stateless)
        .build()
        .await
        .expect("build");
    let running = start(server).await;
    let client = http_client();

    for (path, expected) in [
        ("/api/public/test", "public"),
        ("/api/admin/test", "admin"),
        ("/api/rpc2", "rpc"),
        ("/stateless", "stateless"),
    ] {
        let response = client
            .get(format!("{}{path}", running.base))
            .send()
            .await
            .expect("hook route");
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
        assert_eq!(response.text().await.expect("body"), expected, "{path}");
    }

    running.stop().await;
}

#[tokio::test]
async fn public_router_can_own_ping_and_oauth_without_duplicate_routes() {
    let dir = TempDir::new().expect("temp dir");
    let public = Router::<ApiState>::new()
        .route("/ping", get(|| async { "public-ping" }))
        .route("/api/oauth", get(|| async { "public-oauth" }))
        .route("/api/oauth_callback", get(|| async { "public-callback" }));
    let oauth = OAuthRuntime::with_provider(Arc::new(FakeProvider::new("public")));
    let server = ServerBuilder::new(server_config(&dir))
        .with_db(memory_db())
        .skip_services()
        .with_oauth(oauth)
        .merge_public(public)
        .build()
        .await
        .expect("build");
    assert_eq!(
        server
            .state()
            .oauth_provider()
            .expect("oauth provider")
            .name(),
        "fake"
    );
    let running = start(server).await;
    let client = http_client();

    for (path, expected) in [
        ("/ping", "public-ping"),
        ("/api/oauth", "public-oauth"),
        ("/api/oauth_callback", "public-callback"),
    ] {
        let response = client
            .get(format!("{}{path}", running.base))
            .send()
            .await
            .expect("public route");
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
        assert_eq!(response.text().await.expect("body"), expected, "{path}");
    }

    running.stop().await;
}

#[tokio::test]
async fn reload_services_refreshes_the_oauth_provider_snapshot() {
    let dir = TempDir::new().expect("temp dir");
    let oauth = OAuthRuntime::with_provider(Arc::new(FakeProvider::named("fake-a", "a")));
    let server = ServerBuilder::new(server_config(&dir))
        .with_db(memory_db())
        .skip_services()
        .with_oauth(oauth.clone())
        .build()
        .await
        .expect("build");
    assert_eq!(
        server.state().oauth_provider().expect("provider").name(),
        "fake-a"
    );

    oauth.set_provider(Some(Arc::new(FakeProvider::named("fake-b", "b"))));
    server.reload_services().await;
    assert_eq!(
        server.state().oauth_provider().expect("provider").name(),
        "fake-b"
    );
}

#[tokio::test]
async fn background_tasks_start_and_cancel() {
    let dir = TempDir::new().expect("temp dir");
    let config = ServerConfig {
        background: BackgroundConfig {
            enabled: true,
            record_maintenance_interval: Duration::from_secs(3600),
            notify_interval: Duration::from_secs(3600),
            asset_refresh_interval: Duration::from_secs(3600),
            ddns_interval: Duration::from_secs(3600),
            offline_check_interval: Duration::from_secs(3600),
        },
        ..server_config(&dir)
    };
    let server = build(config, memory_db()).await;
    assert!(server.background().task_count() >= 6);
    assert!(!server.background().is_cancelled());

    server.background().cancel();
    assert!(server.background().is_cancelled());
    server.background().shutdown().await;
    assert_eq!(server.background().task_count(), 0);
}

#[tokio::test]
async fn pending_backup_is_restored_before_the_database_opens() {
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("komari.db");
    let db = Db::open(&db_path).expect("open db");
    db.migrate().expect("migrate");
    std::fs::write(dir.path().join("hello.txt"), "restored").expect("data file");
    let options = BackupOptions::new(dir.path()).with_database_path(&db_path);
    let archive = db.export_backup(&options).expect("export backup");
    drop(db);

    std::fs::remove_file(dir.path().join("hello.txt")).expect("remove original");
    std::fs::write(dir.path().join("backup.zip"), archive).expect("write pending backup");

    let server = ServerBuilder::new(server_config(&dir))
        .skip_services()
        .build()
        .await
        .expect("restore and build");

    assert_eq!(
        std::fs::read_to_string(dir.path().join("hello.txt")).expect("restored file"),
        "restored"
    );
    assert!(!dir.path().join("backup.zip").exists());
    assert_eq!(server.state().db.accounts().count().expect("accounts"), 1);
}

#[tokio::test]
async fn pending_backup_is_archived_before_restore() {
    let root = TempDir::new().expect("temp root");
    let data_dir = root.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("data dir");
    let db_path = data_dir.join("komari.db");
    let db = Db::open(&db_path).expect("open db");
    db.migrate().expect("migrate");
    std::fs::write(data_dir.join("hello.txt"), "restored").expect("data file");
    let options = BackupOptions::new(&data_dir).with_database_path(&db_path);
    let archive = db.export_backup(&options).expect("export backup");
    drop(db);
    std::fs::remove_file(data_dir.join("hello.txt")).expect("remove original");
    std::fs::write(data_dir.join("backup.zip"), archive).expect("write pending backup");

    let config = ServerConfig {
        database_path: db_path,
        data_dir: data_dir.clone(),
        ..server_config(&root)
    };
    ServerBuilder::new(config)
        .skip_services()
        .build()
        .await
        .expect("restore and build");

    let backup_dir = root.path().join("backup");
    let archives: Vec<_> = std::fs::read_dir(&backup_dir)
        .expect("backup dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "zip"))
        .collect();
    assert_eq!(archives.len(), 1, "expected one pre-restore archive");
    assert_eq!(
        std::fs::read_to_string(data_dir.join("hello.txt")).expect("restored file"),
        "restored"
    );
}

#[tokio::test]
async fn pending_backup_restores_a_custom_database_filename() {
    let root = TempDir::new().expect("temp root");
    let data_dir = root.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("data dir");
    let custom_db = root.path().join("custom.db");
    let db = Db::open(&custom_db).expect("open db");
    db.migrate().expect("migrate");
    db.accounts()
        .create("custom-admin", "password")
        .expect("user");
    let options = BackupOptions::new(&data_dir).with_database_path(&custom_db);
    let archive = db.export_backup(&options).expect("export backup");
    drop(db);
    std::fs::remove_file(&custom_db).expect("remove custom db");
    std::fs::write(data_dir.join("backup.zip"), archive).expect("pending backup");

    let config = ServerConfig {
        database_path: custom_db.clone(),
        data_dir: data_dir.clone(),
        ..server_config(&root)
    };
    let server = ServerBuilder::new(config)
        .skip_services()
        .build()
        .await
        .expect("restore and build");

    assert!(custom_db.is_file());
    assert!(!data_dir.join("komari.db").exists());
    assert_eq!(server.state().db.accounts().count().expect("accounts"), 1);
    assert!(
        server
            .state()
            .db
            .accounts()
            .get_by_username("custom-admin")
            .expect("lookup")
            .is_some()
    );
}

#[tokio::test]
async fn graceful_shutdown_completes_and_cancels_background_tasks() {
    let dir = TempDir::new().expect("temp dir");
    let config = ServerConfig {
        background: BackgroundConfig {
            enabled: true,
            record_maintenance_interval: Duration::from_secs(3600),
            notify_interval: Duration::from_secs(3600),
            asset_refresh_interval: Duration::from_secs(3600),
            ddns_interval: Duration::from_secs(3600),
            offline_check_interval: Duration::from_secs(3600),
        },
        ..server_config(&dir)
    };
    let server = build(config, memory_db()).await;
    let background = server.background().cancel_token();
    let state = server.state().clone();
    let running = start(server).await;
    let client = http_client();

    assert_eq!(
        client
            .get(format!("{}/ping", running.base))
            .send()
            .await
            .expect("ping")
            .status(),
        reqwest::StatusCode::OK
    );

    let Running {
        shutdown, handle, ..
    } = running;
    let _ = shutdown.send(());
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("shutdown timeout")
        .expect("server task");
    assert!(result.is_ok(), "{result:?}");
    assert!(background.is_cancelled());
    assert!(state.background_cancelled());
}

#[tokio::test]
async fn graceful_shutdown_times_out_half_open_connections() {
    let dir = TempDir::new().expect("temp dir");
    let config = ServerConfig {
        shutdown_timeout: Duration::from_millis(100),
        ..server_config(&dir)
    };
    let server = build(config, memory_db()).await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind random port");
    let address = listener.local_addr().expect("address");
    let (shutdown, receiver) = oneshot::channel();
    let handle = tokio::spawn(server.serve_with_shutdown(listener, async {
        let _ = receiver.await;
    }));

    // Send headers without the terminating blank line: the connection is
    // accepted but never becomes a complete request.
    let mut stream = TcpStream::connect(address).await.expect("connect");
    stream
        .write_all(b"GET /ping HTTP/1.1\r\nHost: localhost\r\n")
        .await
        .expect("write partial request");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let _ = shutdown.send(());
    let result = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("shutdown timeout")
        .expect("server task");
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn oauth_is_disabled_by_default() {
    let dir = TempDir::new().expect("temp dir");
    let server = build(server_config(&dir), memory_db()).await;
    let running = start(server).await;
    let client = http_client();

    let (status, body) = get_json(&client, &format!("{}/api/oauth", running.base)).await;
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "OAuth is not enabled");

    running.stop().await;
}

struct FakeProvider {
    states: StateStore,
    subject: String,
    provider_name: &'static str,
}

impl FakeProvider {
    fn new(subject: &str) -> Self {
        Self::named("fake", subject)
    }

    fn named(name: &'static str, subject: &str) -> Self {
        Self {
            states: StateStore::default(),
            subject: subject.to_owned(),
            provider_name: name,
        }
    }
}

#[async_trait]
impl OAuthProvider for FakeProvider {
    fn name(&self) -> &'static str {
        self.provider_name
    }

    fn configuration(&self) -> Value {
        serde_json::json!({})
    }

    fn state_store(&self) -> &StateStore {
        &self.states
    }

    async fn init(&self) -> komari_oauth::Result<()> {
        Ok(())
    }

    async fn authorization_url(
        &self,
        redirect_uri: &str,
    ) -> komari_oauth::Result<AuthorizationRequest> {
        let state = self.states.issue(redirect_uri, None, None);
        Ok(AuthorizationRequest {
            url: format!("https://idp.example/authorize?state={state}"),
            state,
            pkce_verifier: None,
        })
    }

    async fn exchange_code(&self, _request: ExchangeRequest<'_>) -> komari_oauth::Result<TokenSet> {
        Err(OAuthError::Unsupported("fake provider".to_owned()))
    }

    async fn fetch_user_info(&self, _tokens: &TokenSet) -> komari_oauth::Result<UserInfo> {
        Err(OAuthError::Unsupported("fake provider".to_owned()))
    }

    async fn callback(&self, request: CallbackRequest<'_>) -> komari_oauth::Result<OAuthUser> {
        self.states.consume(request.state, request.redirect_uri)?;
        Ok(OAuthUser {
            provider: self.provider_name.to_owned(),
            subject: self.subject.clone(),
            username: Some("fake-user".to_owned()),
            ..OAuthUser::default()
        })
    }
}

async fn oauth_server(dir: &TempDir, db: Db, subject: &str) -> Running {
    db.config()
        .set("o_auth_enabled", &true)
        .expect("enable oauth");
    let oauth = OAuthRuntime::with_provider(Arc::new(FakeProvider::new(subject)));
    let server = ServerBuilder::new(server_config(dir))
        .with_db(db)
        .skip_services()
        .with_oauth(oauth)
        .build()
        .await
        .expect("build oauth server");
    start(server).await
}

fn oauth_state(location: &str, cookie: &str) -> String {
    let from_location = location
        .split("state=")
        .nth(1)
        .and_then(|value| value.split('&').next());
    from_location
        .map(str::to_owned)
        .or_else(|| {
            cookie
                .split(';')
                .find_map(|part| part.trim().strip_prefix("oauth_state="))
                .map(str::to_owned)
        })
        .expect("oauth state")
}

async fn oauth_start(running: &Running, client: &reqwest::Client) -> (String, String) {
    let response = client
        .get(format!("{}/api/oauth", running.base))
        .send()
        .await
        .expect("oauth start");
    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .expect("location")
        .to_owned();
    let cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .expect("state cookie")
        .to_owned();
    (location, cookie)
}

#[tokio::test]
async fn oauth_start_redirects_and_sets_the_state_cookie() {
    let dir = TempDir::new().expect("temp dir");
    let running = oauth_server(&dir, memory_db(), "subject-1").await;
    let client = http_client();

    let (location, cookie) = oauth_start(&running, &client).await;
    assert!(location.starts_with("https://idp.example/authorize?state="));
    assert!(cookie.contains("oauth_state="), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("Max-Age=3600"), "{cookie}");

    running.stop().await;
}

#[tokio::test]
async fn oauth_callback_returns_401_for_an_unbound_identity() {
    let dir = TempDir::new().expect("temp dir");
    let running = oauth_server(&dir, memory_db(), "unbound").await;
    let client = http_client();

    let (location, cookie) = oauth_start(&running, &client).await;
    let state = oauth_state(&location, &cookie);
    let response = client
        .get(format!(
            "{}/api/oauth_callback?state={state}&code=the-code",
            running.base
        ))
        .header(COOKIE, format!("oauth_state={state}"))
        .send()
        .await
        .expect("callback");
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = response.json().await.expect("json");
    assert_eq!(
        body["message"],
        "please log in and bind your external account first."
    );

    running.stop().await;
}

#[tokio::test]
async fn oauth_callback_logs_in_a_bound_account() {
    let dir = TempDir::new().expect("temp dir");
    let db = memory_db();
    let user = db.accounts().create("bound", "password").expect("user");
    komari_oauth::AccountLinker::new(&db)
        .bind(
            &user.uuid,
            &komari_oauth::SsoIdentity::new("fake", "bound-subject"),
        )
        .expect("bind");
    let running = oauth_server(&dir, db, "bound-subject").await;
    let client = http_client();

    let (location, cookie) = oauth_start(&running, &client).await;
    let state = oauth_state(&location, &cookie);
    let response = client
        .get(format!(
            "{}/api/oauth_callback?state={state}&code=the-code",
            running.base
        ))
        .header(COOKIE, format!("oauth_state={state}"))
        .send()
        .await
        .expect("callback");
    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    assert_eq!(
        response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some("/admin")
    );
    let cookies: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_owned))
        .collect();
    assert!(
        cookies
            .iter()
            .any(|cookie| cookie.contains("session_token="))
    );
    assert!(
        cookies
            .iter()
            .any(|cookie| cookie.contains("Max-Age=2592000"))
    );
    assert_eq!(running.db.sessions().count().expect("sessions"), 1);

    running.stop().await;
}

#[tokio::test]
async fn oauth_callback_binds_an_external_account_to_the_session_user() {
    let dir = TempDir::new().expect("temp dir");
    let db = memory_db();
    let user = db.accounts().create("binding", "password").expect("user");
    let session = komari_auth::create_session(&db, &user.uuid, "test", "127.0.0.1", "password")
        .expect("session");
    let running = oauth_server(&dir, db, "binding-subject").await;
    let client = http_client();

    let (location, cookie) = oauth_start(&running, &client).await;
    let state = oauth_state(&location, &cookie);
    let response = client
        .get(format!(
            "{}/api/oauth_callback?state={state}&code=the-code",
            running.base
        ))
        .header(
            COOKIE,
            format!(
                "oauth_state={state}; binding_external_account={}; session_token={session}",
                user.uuid
            ),
        )
        .send()
        .await
        .expect("callback");
    assert_eq!(response.status(), reqwest::StatusCode::FOUND);

    let stored = running
        .db
        .accounts()
        .require_by_uuid(&user.uuid)
        .expect("stored user");
    assert_eq!(stored.sso_id, "fake_binding-subject");
    assert_eq!(stored.sso_type, "fake");

    running.stop().await;
}

#[tokio::test]
async fn oauth_callback_rejects_a_mismatched_state() {
    let dir = TempDir::new().expect("temp dir");
    let running = oauth_server(&dir, memory_db(), "subject-1").await;
    let client = http_client();

    let response = client
        .get(format!(
            "{}/api/oauth_callback?state=wrong&code=the-code",
            running.base
        ))
        .header(COOKIE, "oauth_state=different")
        .send()
        .await
        .expect("callback");
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.expect("json");
    assert_eq!(body["error"], "Invalid state");

    running.stop().await;
}
