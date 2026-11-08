//! Integration tests for the public/browser API (`komari_api::public`).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use futures_util::StreamExt;
use komari_api::{
    ApiConfig, ApiState, Report, client_router, public_router, public_router_with_state,
};
use komari_db::models::{GPURecord, Record};
use komari_db::{ClientPatch, Db};
use serde_json::{Value, json};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn memory_db() -> Db {
    let db = Db::open_in_memory().expect("open in-memory database");
    db.migrate().expect("migrate database");
    db
}

fn state() -> ApiState {
    let config = ApiConfig {
        presence_sweep_interval: Duration::ZERO,
        report_flush_interval: Duration::ZERO,
        // Keep the MJPEG test fast; production defaults to 2 seconds.
        mjpeg_frame_interval: Duration::from_millis(10),
        // Never touch the network in tests; the bitmap fallback is used.
        mjpeg_font_download: false,
        ..ApiConfig::default()
    };
    ApiState::with_config(memory_db(), config)
}

fn app(state: &ApiState) -> Router {
    public_router_with_state(state.clone())
}

fn request(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("build request")
}

fn get(uri: &str) -> Request<Body> {
    request(Method::GET, uri)
}

fn post_json(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("build request")
}

fn with_cookie(mut request: Request<Body>, cookie: &str) -> Request<Body> {
    request
        .headers_mut()
        .insert(COOKIE, cookie.parse().expect("valid cookie header"));
    request
}

fn with_peer(mut request: Request<Body>, address: SocketAddr) -> Request<Body> {
    request.extensions_mut().insert(ConnectInfo(address));
    request
}

async fn call(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, headers, value)
}

fn set_cookie_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|cookie| cookie.starts_with(&format!("{name}=")))
        .map(str::to_owned)
}

fn create_session(state: &ApiState, username: &str) -> (String, String) {
    let user = state
        .db
        .accounts()
        .create(username, "password")
        .expect("create user");
    let session =
        komari_auth::create_session(&state.db, &user.uuid, "test-agent", "127.0.0.1", "password")
            .expect("create session");
    (user.uuid, session)
}

fn set_config_bool(state: &ApiState, key: &str, value: bool) {
    state
        .db
        .config()
        .set(key, &value)
        .expect("store configuration");
}

fn set_config_string(state: &ApiState, key: &str, value: &str) {
    state
        .db
        .config()
        .set(key, &value)
        .expect("store configuration");
}

fn set_config_i64(state: &ApiState, key: &str, value: i64) {
    state
        .db
        .config()
        .set(key, &value)
        .expect("store configuration");
}

// ---------------------------------------------------------------------------
// Nodes
// ---------------------------------------------------------------------------

fn create_client(state: &ApiState, name: &str, hidden: bool) -> String {
    let client = state
        .db
        .clients()
        .create_named(name)
        .expect("create client");
    state
        .db
        .clients()
        .patch(
            &client.uuid,
            &ClientPatch {
                hidden: Some(hidden),
                ipv4: Some("203.0.113.1".to_owned()),
                ipv6: Some("2001:db8::1".to_owned()),
                remark: Some("private note".to_owned()),
                public_remark: Some("public note".to_owned()),
                version: Some("1.2.3".to_owned()),
                ..ClientPatch::default()
            },
        )
        .expect("patch client");
    client.uuid
}

#[tokio::test]
async fn public_settings_use_go_defaults_when_config_is_empty() {
    let state = state();
    let (status, _, body) = call(&app(&state), get("/api/public")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
    let data = &body["data"];
    assert_eq!(data["sitename"], "Komari");
    assert_eq!(data["description"], "A simple server monitor tool.");
    assert_eq!(data["disable_password_login"], false);
    assert_eq!(data["allow_cors"], false);
    assert_eq!(data["record_enabled"], true);
    assert_eq!(data["record_preserve_time"], 720);
    assert_eq!(data["private_site"], false);
    assert_eq!(data["theme"], "default");
    assert_eq!(data["theme_settings"], json!({}));

    // Go `GetManyAs` writes defaults back to `configs` on the first call.
    for key in [
        "sitename",
        "description",
        "custom_head",
        "custom_body",
        "disable_password_login",
        "allow_cors",
        "record_enabled",
        "record_preserve_time",
        "private_site",
        "theme",
    ] {
        assert!(
            state.db.config().contains(key).expect("config lookup"),
            "missing persisted default for {key}"
        );
    }
}

#[tokio::test]
async fn nodes_filters_hidden_clients_and_clears_private_fields() {
    let state = state();
    let visible = create_client(&state, "visible", false);
    let hidden = create_client(&state, "hidden", true);

    let (status, _, body) = call(&app(&state), get("/api/nodes")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
    let nodes = body["data"].as_array().expect("node array");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["uuid"], visible);
    assert_eq!(nodes[0]["name"], "visible");
    assert_eq!(nodes[0]["public_remark"], "public note");
    for field in ["ipv4", "ipv6", "remark", "version", "token"] {
        assert!(
            nodes[0].get(field).is_none(),
            "guest node response must not expose {field}"
        );
    }
    assert!(nodes.iter().all(|node| node["uuid"] != hidden));
}

#[tokio::test]
async fn nodes_include_hidden_clients_for_a_logged_in_session() {
    let state = state();
    let hidden = create_client(&state, "hidden", true);
    let (_, session) = create_session(&state, "admin");

    let request = with_cookie(get("/api/nodes"), &format!("session_token={session}"));
    let (status, _, body) = call(&app(&state), request).await;
    assert_eq!(status, StatusCode::OK);
    let nodes = body["data"].as_array().expect("node array");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["uuid"], hidden);
    assert!(nodes[0].get("ipv4").is_none());
}

// ---------------------------------------------------------------------------
// Public settings and version
// ---------------------------------------------------------------------------

#[tokio::test]
async fn nodes_prefer_the_session_cookie_over_an_api_key() {
    let state = state();
    let hidden = create_client(&state, "hidden", true);
    let (_, session) = create_session(&state, "admin");
    set_config_string(&state, "api_key", "0123456789abcdef");

    // An API key alone is not a browser session: hidden clients stay hidden.
    let mut api_key_request = get("/api/nodes");
    api_key_request.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        "Bearer 0123456789abcdef".parse().expect("header"),
    );
    let (_, _, body) = call(&app(&state), api_key_request).await;
    assert_eq!(body["data"], json!([]));

    // With both credentials present, Go still reads the session cookie.
    let mut both = with_cookie(get("/api/nodes"), &format!("session_token={session}"));
    both.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        "Bearer 0123456789abcdef".parse().expect("header"),
    );
    let (status, _, body) = call(&app(&state), both).await;
    assert_eq!(status, StatusCode::OK);
    let nodes = body["data"].as_array().expect("node array");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["uuid"], hidden);
}

#[tokio::test]
async fn public_settings_read_config_and_honour_temporary_access() {
    let state = state();
    set_config_string(&state, "sitename", "My Site");
    set_config_string(&state, "description", "Custom description");
    set_config_bool(&state, "private_site", true);
    set_config_bool(&state, "record_enabled", false);
    set_config_i64(&state, "record_preserve_time", 48);
    set_config_string(&state, "theme", "custom");
    state
        .db
        .themes()
        .set("custom", &json!({ "accent": "blue" }))
        .expect("store theme settings");
    set_config_string(&state, "tempory_share_token", "share-token");
    set_config_i64(&state, "tempory_share_token_expire_at", now_unix() + 600);

    let (_, _, body) = call(&app(&state), get("/api/public")).await;
    assert_eq!(body["data"]["sitename"], "My Site");
    assert_eq!(body["data"]["description"], "Custom description");
    assert_eq!(body["data"]["private_site"], true);
    assert_eq!(body["data"]["record_enabled"], false);
    assert_eq!(body["data"]["record_preserve_time"], 48);
    assert_eq!(body["data"]["theme_settings"]["accent"], "blue");

    let request = with_cookie(get("/api/public"), "temp_key=share-token");
    let (_, _, body) = call(&app(&state), request).await;
    assert_eq!(body["data"]["private_site"], false);

    let request = with_cookie(get("/api/public"), "temp_key=wrong-token");
    let (_, _, body) = call(&app(&state), request).await;
    assert_eq!(body["data"]["private_site"], true);
}

#[tokio::test]
async fn version_returns_the_build_constants() {
    let state = state();
    let (status, _, body) = call(&app(&state), get("/api/version")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
    assert_eq!(body["data"]["version"], komari_api::public::VERSION);
    assert_eq!(body["data"]["hash"], komari_api::public::VERSION_HASH);
}

// ---------------------------------------------------------------------------
// Login / logout / me
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_creates_a_session_and_sets_the_go_cookie() {
    let state = state();
    state.db.accounts().create("admin", "secret").expect("user");

    let (status, headers, body) = call(
        &app(&state),
        post_json(
            "/api/login",
            json!({ "username": "admin", "password": "secret" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
    let token = body["data"]["set-cookie"]["session_token"]
        .as_str()
        .expect("session token");
    assert_eq!(token.len(), 32);

    let cookie = set_cookie_header(&headers, "session_token").expect("session cookie");
    assert!(cookie.contains("Path=/"));
    assert!(cookie.contains("Max-Age=2592000"));
    assert!(cookie.contains("HttpOnly"));
    assert_eq!(state.db.sessions().count().expect("session count"), 1);
}

#[tokio::test]
async fn login_rejects_bad_credentials_and_missing_fields() {
    let state = state();
    state.db.accounts().create("admin", "secret").expect("user");

    let (status, _, body) = call(
        &app(&state),
        post_json(
            "/api/login",
            json!({ "username": "admin", "password": "wrong" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["status"], "error");
    assert_eq!(body["message"], "Invalid credentials");

    let (status, _, body) = call(
        &app(&state),
        post_json("/api/login", json!({ "username": "admin" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["message"],
        "Invalid request body: Username and password are required"
    );
}

#[tokio::test]
async fn login_is_forbidden_when_password_login_is_disabled() {
    let state = state();
    state.db.accounts().create("admin", "secret").expect("user");
    set_config_bool(&state, "disable_password_login", true);

    let (status, _, body) = call(
        &app(&state),
        post_json(
            "/api/login",
            json!({ "username": "admin", "password": "secret" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["message"], "Password login is disabled");
}

#[tokio::test]
async fn login_requires_and_verifies_a_2fa_code() {
    let state = state();
    let user = state.db.accounts().create("admin", "secret").expect("user");
    let secret = komari_auth::generate_secret();
    komari_auth::enable_2fa(&state.db, &user.uuid, &secret).expect("enable 2fa");

    let (status, _, body) = call(
        &app(&state),
        post_json(
            "/api/login",
            json!({ "username": "admin", "password": "secret" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["message"], "2FA code is required");

    let (status, _, body) = call(
        &app(&state),
        post_json(
            "/api/login",
            json!({ "username": "admin", "password": "secret", "2fa_code": "000000" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["message"], "Invalid 2FA code");

    let code = komari_auth::generate_code_at(&secret, now_unix()).expect("totp code");
    let (status, _, body) = call(
        &app(&state),
        post_json(
            "/api/login",
            json!({ "username": "admin", "password": "secret", "2fa_code": code }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
}

#[tokio::test]
async fn me_reports_guest_and_logged_in_states() {
    let state = state();
    let (status, _, body) = call(&app(&state), get("/api/me")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "username": "Guest", "logged_in": false }));

    let (uuid, session) = create_session(&state, "admin");
    let request = with_cookie(get("/api/me"), &format!("session_token={session}"));
    let (status, _, body) = call(&app(&state), request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], "admin");
    assert_eq!(body["logged_in"], true);
    assert_eq!(body["uuid"], uuid);
    assert_eq!(body["2fa_enabled"], false);

    let request = with_cookie(get("/api/me"), "session_token=does-not-exist");
    let (_, _, body) = call(&app(&state), request).await;
    assert_eq!(body["username"], "Guest");
    assert_eq!(body["logged_in"], false);
}

#[tokio::test]
async fn me_treats_an_expired_session_as_a_guest_and_deletes_it() {
    let state = state();
    let user = state.db.accounts().create("admin", "secret").expect("user");
    let session = komari_auth::create_session_with_expiry(
        &state.db,
        &user.uuid,
        -1,
        "test-agent",
        "127.0.0.1",
        "password",
    )
    .expect("expired session");
    assert_eq!(state.db.sessions().count().expect("session count"), 1);

    let request = with_cookie(get("/api/me"), &format!("session_token={session}"));
    let (status, _, body) = call(&app(&state), request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "username": "Guest", "logged_in": false }));
    assert_eq!(state.db.sessions().count().expect("session count"), 0);
}

#[tokio::test]
async fn login_uses_the_peer_address_and_writes_an_audit_row() {
    let state = state();
    state.db.accounts().create("admin", "secret").expect("user");
    let request = with_peer(
        post_json(
            "/api/login",
            json!({ "username": "admin", "password": "secret" }),
        ),
        SocketAddr::from(([203, 0, 113, 9], 4444)),
    );
    let (status, _, _) = call(&app(&state), request).await;
    assert_eq!(status, StatusCode::OK);

    let sessions = state.db.sessions().list().expect("sessions");
    assert_eq!(sessions[0].ip, "203.0.113.9");

    let logs = state.db.logs().list(10).expect("logs");
    let entry = logs
        .iter()
        .find(|entry| entry.msg_type == "login")
        .expect("login audit row");
    assert_eq!(entry.message, "logged in (password)");
    assert_eq!(entry.ip, "203.0.113.9");
    assert_eq!(entry.uuid, sessions[0].uuid);
}

#[tokio::test]
async fn forwarded_headers_take_precedence_over_the_peer_address() {
    let state = state();
    let (_, session) = create_session(&state, "admin");
    let mut request = with_peer(
        with_cookie(get("/api/logout"), &format!("session_token={session}")),
        SocketAddr::from(([203, 0, 113, 9], 4444)),
    );
    request.headers_mut().insert(
        "x-forwarded-for",
        "198.51.100.7, 10.0.0.1".parse().expect("header"),
    );
    let (status, _, _) = call(&app(&state), request).await;
    assert_eq!(status, StatusCode::FOUND);

    let logs = state.db.logs().list(10).expect("logs");
    let entry = logs
        .iter()
        .find(|entry| entry.msg_type == "logout")
        .expect("logout audit row");
    assert_eq!(entry.message, "logged out");
    assert_eq!(entry.ip, "198.51.100.7");
}

#[tokio::test]
async fn logout_deletes_the_session_and_clears_the_cookie() {
    let state = state();
    let (_, session) = create_session(&state, "admin");

    let request = with_cookie(get("/api/logout"), &format!("session_token={session}"));
    let (status, headers, _) = call(&app(&state), request).await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(headers.get(LOCATION).expect("location"), "/");
    let cookie = set_cookie_header(&headers, "session_token").expect("clear cookie");
    assert!(cookie.contains("Max-Age=0"));
    assert_eq!(state.db.sessions().count().expect("session count"), 0);
}

// ---------------------------------------------------------------------------
// Recent and historical records
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recent_returns_buffered_reports_and_hides_hidden_clients() {
    let state = state();
    let visible = create_client(&state, "visible", false);
    let hidden = create_client(&state, "hidden", true);

    let mut report = Report {
        uuid: visible.clone(),
        cpu: komari_api::CpuReport {
            usage: 12.5,
            ..Default::default()
        },
        ..Report::default()
    };
    report.mark_updated();
    state.report_cache.record(&visible, report);

    let (status, _, body) = call(&app(&state), get(&format!("/api/recent/{visible}"))).await;
    assert_eq!(status, StatusCode::OK);
    let records = body["data"].as_array().expect("report array");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["uuid"], visible);
    assert_eq!(records[0]["cpu"]["usage"], 12.5);

    let (status, _, body) = call(&app(&state), get(&format!("/api/recent/{hidden}"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "UUID is required");

    let (_, session) = create_session(&state, "admin");
    let request = with_cookie(
        get(&format!("/api/recent/{hidden}")),
        &format!("session_token={session}"),
    );
    let (status, _, body) = call(&app(&state), request).await;
    assert_eq!(status, StatusCode::OK);
    // No buffered report for the hidden client: Go omits `data` entirely.
    assert_eq!(body["status"], "success");
    assert!(body.get("data").is_none());
}

#[tokio::test]
async fn records_load_returns_the_go_envelope_and_gpu_devices() {
    let state = state();
    let uuid = create_client(&state, "visible", false);

    let mut record = Record::new(uuid.clone());
    record.cpu = 42.5;
    record.ram = 512;
    record.ram_total = 1024;
    record.disk = 10;
    record.disk_total = 20;
    record.connections = 9;
    record.connections_udp = 4;
    state.db.records().insert(&record).expect("insert record");

    state
        .db
        .records()
        .insert_gpu(&GPURecord {
            client: uuid.clone(),
            time: record.time,
            device_index: 0,
            device_name: "Mock GPU".to_owned(),
            mem_total: 1024,
            mem_used: 256,
            utilization: 25.0,
            temperature: 50,
        })
        .expect("insert gpu record");

    let (status, _, body) =
        call(&app(&state), get(&format!("/api/records/load?uuid={uuid}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
    assert_eq!(body["data"]["count"], 1);
    assert_eq!(body["data"]["records"][0]["client"], uuid);
    assert_eq!(body["data"]["has_gpu_data"], true);
    assert_eq!(body["data"]["gpu_devices"]["0"]["device_name"], "Mock GPU");
    assert_eq!(
        body["data"]["gpu_devices"]["0"]["records"]
            .as_array()
            .expect("gpu records")
            .len(),
        1
    );

    let (_, _, body) = call(
        &app(&state),
        get(&format!("/api/records/load?uuid={uuid}&load_type=cpu")),
    )
    .await;
    assert_eq!(body["data"]["load_type"], "cpu");
    assert_eq!(body["data"]["records"][0]["cpu"], 42.5);
    assert!(body["data"]["records"][0].get("ram").is_none());

    let (_, _, body) = call(
        &app(&state),
        get(&format!("/api/records/load?uuid={uuid}&load_type=ram")),
    )
    .await;
    assert_eq!(body["data"]["records"][0]["ram"], 512);
    assert_eq!(body["data"]["records"][0]["ram_total"], 1024);
    assert_eq!(body["data"]["records"][0]["ram_percent"], 50.0);

    let (_, _, body) = call(
        &app(&state),
        get(&format!(
            "/api/records/load?uuid={uuid}&load_type=connections"
        )),
    )
    .await;
    assert_eq!(body["data"]["records"][0]["connections_tcp"], 5);
}

#[tokio::test]
async fn records_load_validates_parameters_and_hidden_clients() {
    let state = state();
    let hidden = create_client(&state, "hidden", true);

    let (status, _, body) = call(&app(&state), get("/api/records/load")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "UUID is required");

    for load_type in ["nope", "gpu"] {
        let (status, _, body) = call(
            &app(&state),
            get(&format!(
                "/api/records/load?uuid=some-uuid&load_type={load_type}"
            )),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["message"], "Invalid load_type parameter");
    }

    let (status, _, body) = call(
        &app(&state),
        get("/api/records/load?uuid=some-uuid&hours=abc"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "Invalid hours parameter");

    let (status, _, body) = call(
        &app(&state),
        get(&format!("/api/records/load?uuid={hidden}")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "UUID is required");
}

#[tokio::test]
async fn records_load_uses_go_f32_formatting_and_nulls_empty_results() {
    let state = state();
    let uuid = create_client(&state, "visible", false);

    // No rows and no load_type filter: Go's nil slice encodes as `null`.
    let (_, _, body) = call(&app(&state), get(&format!("/api/records/load?uuid={uuid}"))).await;
    assert!(body["data"]["records"].is_null());
    assert_eq!(body["data"]["count"], 0);

    let mut record = Record::new(uuid.clone());
    record.cpu = 12.3;
    record.ram = 1;
    record.ram_total = 3;
    state.db.records().insert(&record).expect("insert record");

    let (_, _, body) = call(
        &app(&state),
        get(&format!("/api/records/load?uuid={uuid}&load_type=cpu")),
    )
    .await;
    assert_eq!(body["data"]["records"][0]["cpu"], json!(12.3));

    let (_, _, body) = call(&app(&state), get(&format!("/api/records/load?uuid={uuid}"))).await;
    assert_eq!(body["data"]["records"][0]["cpu"], json!(12.3));

    let (_, _, body) = call(
        &app(&state),
        get(&format!("/api/records/load?uuid={uuid}&load_type=ram")),
    )
    .await;
    // Go encodes the `float32` value with its shortest form ("33.333336").
    let expected: f64 = (1.0f32 / 3.0f32 * 100.0)
        .to_string()
        .parse()
        .expect("f32 text");
    assert_eq!(
        body["data"]["records"][0]["ram_percent"]
            .as_f64()
            .expect("number"),
        expected
    );
}

#[tokio::test]
async fn records_load_rejects_extreme_hours_and_wraps_connections() {
    let state = state();
    let uuid = create_client(&state, "visible", false);
    let mut record = Record::new(uuid.clone());
    record.connections = 2;
    record.connections_udp = 5;
    state.db.records().insert(&record).expect("insert record");

    for hours in ["3000000", "9223372036854775807", "-9223372036854775808"] {
        let (status, _, body) = call(
            &app(&state),
            get(&format!("/api/records/load?uuid={uuid}&hours={hours}")),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "hours={hours}");
        assert_eq!(body["message"], "Invalid hours parameter");
    }

    let (_, _, body) = call(
        &app(&state),
        get(&format!(
            "/api/records/load?uuid={uuid}&load_type=connections"
        )),
    )
    .await;
    assert_eq!(body["data"]["records"][0]["connections_tcp"], -3);
}

// ---------------------------------------------------------------------------
// OAuth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn public_and_agent_routers_compose_on_one_state() {
    let state = state();
    let router = Router::new()
        .merge(public_router())
        .merge(client_router())
        .with_state(state);

    let (status, _, body) = call(&router, get("/ping")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!("pong"));

    let (status, _, body) = call(&router, get("/api/version")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");

    let (status, _, body) = call(&router, get("/api/nodes")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"], json!([]));
}

#[tokio::test]
async fn private_site_blocks_guest_api_paths_but_allows_public_prefixes() {
    let state = state();
    set_config_bool(&state, "private_site", true);

    let (status, _, body) = call(&app(&state), get("/api/nodes")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body["message"],
        "Private site is enabled, please login first."
    );

    for path in ["/api/public", "/api/version", "/api/login"] {
        let (status, _, _) = call(&app(&state), get(path)).await;
        assert_ne!(status, StatusCode::UNAUTHORIZED, "{path}");
    }

    let (status, _, _) = call(&app(&state), get("/api/records/load")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (_, session) = create_session(&state, "admin");
    let request = with_cookie(get("/api/nodes"), &format!("session_token={session}"));
    let (status, _, _) = call(&app(&state), request).await;
    assert_eq!(status, StatusCode::OK);
}

static MJPEG_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn split_frame(frame: &[u8]) -> (&str, &[u8]) {
    let header_end = frame
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("multipart header terminator")
        + 4;
    let payload = frame[header_end..]
        .strip_suffix(b"\r\n")
        .expect("multipart part trailer");
    (
        std::str::from_utf8(&frame[..header_end]).expect("multipart header text"),
        payload,
    )
}

fn part_content_length(header: &str) -> usize {
    header
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .expect("part Content-Length")
        .trim()
        .parse()
        .expect("numeric Content-Length")
}

fn decode_rgb(payload: &[u8]) -> image::RgbImage {
    image::load_from_memory(payload)
        .expect("decode JPEG payload")
        .to_rgb8()
}

fn non_white(image: &image::RgbImage) -> usize {
    image
        .pixels()
        .filter(|pixel| pixel.0 != [255, 255, 255])
        .count()
}

fn has_color_near(image: &image::RgbImage, target: [u8; 3], tolerance: i16) -> bool {
    image.pixels().any(|pixel| {
        pixel
            .0
            .iter()
            .zip(target)
            .all(|(value, expected)| (*value as i16 - expected as i16).abs() <= tolerance)
    })
}

fn assert_stream_headers(headers: &HeaderMap) {
    assert_eq!(
        headers
            .get(CONTENT_TYPE)
            .expect("content type")
            .to_str()
            .expect("header text"),
        "multipart/x-mixed-replace; boundary=frame"
    );
    assert_eq!(
        headers
            .get("cache-control")
            .expect("cache control")
            .to_str()
            .expect("header text"),
        "no-cache"
    );
    assert_eq!(
        headers
            .get("connection")
            .expect("connection")
            .to_str()
            .expect("header text"),
        "keep-alive"
    );
}

async fn first_frame(app: &Router, uri: &str) -> (HeaderMap, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(get(uri))
        .await
        .expect("router response");
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    let headers = response.headers().clone();
    assert_stream_headers(&headers);
    let mut stream = response.into_body().into_data_stream();
    let frame = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("frame within timeout")
        .expect("stream item")
        .expect("frame bytes");
    drop(stream);
    let (header, payload) = split_frame(&frame);
    assert_eq!(part_content_length(header), payload.len());
    assert_eq!(&payload[..3], &[0xFF, 0xD8, 0xFF]);
    (headers, frame.to_vec())
}

#[tokio::test]
async fn mjpeg_streams_multipart_jpeg_frames_until_the_client_disconnects() {
    let _guard = MJPEG_TEST_LOCK.lock().await;
    // Keep `data/font.ttf` out of the picture so the deterministic bitmap
    // fallback is used even on a developer machine with a downloaded font.
    let data_dir = tempfile::tempdir().expect("temp data dir");
    let state = state().with_web_assets(Arc::new(komari_web::WebAssets::with_data_dir(
        data_dir.path(),
    )));
    create_client(&state, "visible", false);
    create_client(&state, "hidden", true);

    let response = app(&state)
        .oneshot(get("/api/mjpeg_live"))
        .await
        .expect("router response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_stream_headers(response.headers());

    let mut stream = response.into_body().into_data_stream();
    let mut last_frame = Vec::new();
    for frame_number in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("frame within timeout")
            .expect("stream item")
            .expect("frame bytes");
        let (header, payload) = split_frame(&frame);
        assert!(
            header.starts_with("--frame\r\nContent-Type: image/jpeg\r\n"),
            "frame {frame_number} part header"
        );
        assert_eq!(
            part_content_length(header),
            payload.len(),
            "frame {frame_number} Content-Length"
        );
        assert_eq!(
            &payload[..3],
            &[0xFF, 0xD8, 0xFF],
            "frame {frame_number} JPEG SOI magic"
        );
        let image = decode_rgb(payload);
        // One visible node (the hidden client is filtered) and the bitmap
        // fallback: Go's `renderStatusTableWithBasicFont` adds 30 px.
        assert_eq!(image.dimensions(), (1280, 252));
        assert!(non_white(&image) > 1000, "frame {frame_number} is blank");
        // No hub report: the row is offline, so red text is present and the
        // online green is absent.
        assert!(
            has_color_near(&image, [200, 0, 0], 60),
            "offline row is missing"
        );
        assert!(
            !has_color_near(&image, [0, 150, 0], 30),
            "unexpected online row"
        );
        last_frame = frame.to_vec();
    }
    assert!(!last_frame.is_empty());

    let rendered = komari_api::public::mjpeg_rendered_frame_count();
    drop(stream);
    // Give the unfold stream several frame intervals to prove it stopped
    // rendering after the body was dropped.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        komari_api::public::mjpeg_rendered_frame_count(),
        rendered,
        "dropping the body must cancel rendering"
    );
}

#[tokio::test]
async fn mjpeg_uses_the_ttf_font_when_data_font_ttf_exists() {
    let _guard = MJPEG_TEST_LOCK.lock().await;
    let data_dir = tempfile::tempdir().expect("temp data dir");
    std::fs::write(
        data_dir.path().join("font.ttf"),
        include_bytes!("fixtures/misans-subset.ttf"),
    )
    .expect("write fixture font");
    let state = state().with_web_assets(Arc::new(komari_web::WebAssets::with_data_dir(
        data_dir.path(),
    )));
    state
        .db
        .config()
        .set(komari_config::keys::SITENAME_KEY, "测试节点")
        .expect("set sitename");
    create_client(&state, "visible", false);
    let router = app(&state);

    let (_, frame) = first_frame(&router, "/api/mjpeg_live?lang=zh_cn&tz_offset=8").await;
    let (header, payload) = split_frame(&frame);
    assert_eq!(part_content_length(header), payload.len());
    let chinese = decode_rgb(payload);
    // The TTF path has no bitmap-fallback warning, so the table is 222 px.
    assert_eq!(chinese.dimensions(), (1280, 222));
    assert!(non_white(&chinese) > 1000, "TTF frame is blank");

    let (_, english_frame) = first_frame(&router, "/api/mjpeg_live?lang=en&tz_offset=-5").await;
    let (_, english_payload) = split_frame(&english_frame);
    let english = decode_rgb(english_payload);
    assert_eq!(english.dimensions(), (1280, 222));
    assert!(
        chinese.as_raw() != english.as_raw(),
        "EN/ZH frames must differ"
    );
}

#[tokio::test]
async fn mjpeg_language_and_tz_parameters_change_the_frame() {
    let _guard = MJPEG_TEST_LOCK.lock().await;
    let data_dir = tempfile::tempdir().expect("temp data dir");
    std::fs::write(
        data_dir.path().join("font.ttf"),
        include_bytes!("fixtures/misans-subset.ttf"),
    )
    .expect("write fixture font");
    let state = state().with_web_assets(Arc::new(komari_web::WebAssets::with_data_dir(
        data_dir.path(),
    )));
    create_client(&state, "visible", false);
    let router = app(&state);

    let (_, english_frame) = first_frame(&router, "/api/mjpeg_live?lang=en").await;
    let (_, chinese_frame) = first_frame(&router, "/api/mjpeg_live?lang=zh_cn").await;
    let (_, english_payload) = split_frame(&english_frame);
    let (_, chinese_payload) = split_frame(&chinese_frame);
    let english = decode_rgb(english_payload);
    let chinese = decode_rgb(chinese_payload);
    assert_eq!(english.dimensions(), (1280, 222));
    assert_eq!(chinese.dimensions(), (1280, 222));
    assert!(
        english.as_raw() != chinese.as_raw(),
        "language must change the rendered headers"
    );

    let (_, east_frame) = first_frame(&router, "/api/mjpeg_live?lang=en&tz_offset=8").await;
    let (_, west_frame) = first_frame(&router, "/api/mjpeg_live?lang=en&tz_offset=-5").await;
    let (_, east_payload) = split_frame(&east_frame);
    let (_, west_payload) = split_frame(&west_frame);
    let east = decode_rgb(east_payload);
    let west = decode_rgb(west_payload);
    // The table above the footer is identical; only the timestamp line moves.
    let table_bytes = 152 * 1280 * 3;
    assert_eq!(&east.as_raw()[..table_bytes], &west.as_raw()[..table_bytes]);
    assert_ne!(
        east.as_raw(),
        west.as_raw(),
        "tz_offset must change the footer"
    );
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_secs() as i64
}
