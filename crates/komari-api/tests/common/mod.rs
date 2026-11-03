//! Shared helpers for the `komari-api` integration tests.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use komari_api::{ApiConfig, ApiState, client_router_with_state};
use komari_db::models::Client;
use komari_db::models::Record;
use komari_db::repositories::records::RecordTable;
use komari_db::{Db, LocalTime};
use serde_json::Value;
use tower::ServiceExt;

pub const AUTO_KEY: &str = "0123456789abcdef";

pub fn memory_db() -> Db {
    let db = Db::open_in_memory().expect("open in-memory database");
    db.migrate().expect("migrate database");
    db
}

pub fn default_config() -> ApiConfig {
    ApiConfig {
        presence_ttl: Duration::from_secs(11),
        socket_read_timeout: Duration::from_secs(11),
        // Keep background work out of deterministic tests; tests drive
        // presence expiry and report flushing explicitly.
        presence_sweep_interval: Duration::ZERO,
        report_flush_interval: Duration::ZERO,
        ..ApiConfig::default()
    }
}

pub fn app() -> (ApiState, Router) {
    app_with(default_config())
}

pub fn app_with(config: ApiConfig) -> (ApiState, Router) {
    let db = memory_db();
    let state = ApiState::with_config(db, config);
    let router = client_router_with_state(state.clone());
    (state, router)
}

pub fn app_with_clock(
    config: ApiConfig,
    clock: Arc<komari_api::ManualClock>,
) -> (ApiState, Router) {
    let db = memory_db();
    let state = ApiState::with_clock(db, config, clock);
    let router = client_router_with_state(state.clone());
    (state, router)
}

/// State plus router for the admin API, with an isolated data directory.
pub fn admin_app(data_dir: &std::path::Path) -> (ApiState, Router) {
    let db = memory_db();
    let state = ApiState::with_config(db, default_config())
        .with_web_assets(Arc::new(komari_web::WebAssets::with_data_dir(data_dir)));
    let router = komari_api::admin::admin_router_with_state(state.clone());
    (state, router)
}

/// State plus router for `/api/rpc2`.
pub fn rpc_app() -> (ApiState, Router) {
    let db = memory_db();
    let state = ApiState::with_config(db, default_config());
    let router = komari_api::rpc::rpc_router_with_state(state.clone());
    (state, router)
}

pub fn set_auto_key(state: &ApiState, key: &str) {
    state
        .db
        .config()
        .set(komari_config::keys::AUTO_DISCOVERY_KEY_KEY, &key)
        .expect("store auto discovery key");
}

pub fn flush(state: &ApiState) -> komari_api::FlushSummary {
    state.flush_reports().expect("flush reports")
}

pub fn create_admin_session(state: &ApiState) -> String {
    let user = state
        .db
        .accounts()
        .create("admin", "password")
        .expect("create admin");
    komari_auth::create_session(&state.db, &user.uuid, "test-agent", "127.0.0.1", "password")
        .expect("create session")
}

pub fn set_api_key(state: &ApiState, key: &str) {
    state
        .db
        .config()
        .set("api_key", &key)
        .expect("store api key");
}

pub fn create_client(state: &ApiState, name: &str) -> Client {
    state
        .db
        .clients()
        .create_named(name)
        .expect("create client")
}

pub fn report_body(uuid: &str) -> Value {
    serde_json::json!({
        "uuid": uuid,
        "cpu": {"name": "test cpu", "cores": 4, "arch": "x86_64", "usage": 12.5},
        "ram": {"used": 1024, "total": 2048},
        "swap": {"used": 0, "total": 1024},
        "load": {"load1": 0.5, "load5": 0.4, "load15": 0.3},
        "disk": {"used": 100, "total": 200},
        "network": {"up": 11, "down": 22, "totalUp": 33, "totalDown": 44},
        "connections": {"tcp": 3, "udp": 4},
        "process": 5,
        "uptime": 100,
    })
}

pub fn request(method: Method, uri: &str, body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.into())
        .expect("build request")
}

pub fn bearer_request(
    method: Method,
    uri: &str,
    token: &str,
    body: impl Into<Body>,
) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(body.into())
        .expect("build request")
}

pub async fn call(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, value)
}

pub fn gpu_rows_for(db: &Db, uuid: &str) -> i64 {
    db.with_connection(|conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM gpu_records WHERE client = ?1",
            [uuid],
            |row| row.get(0),
        )?)
    })
    .expect("count gpu rows")
}

pub fn record_rows_for(db: &Db, uuid: &str) -> i64 {
    db.with_connection(|conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM records WHERE client = ?1",
            [uuid],
            |row| row.get(0),
        )?)
    })
    .expect("count records")
}

pub fn records_for(state: &ApiState, uuid: &str) -> Vec<Record> {
    let now = LocalTime::now().timestamp().expect("timestamp");
    let start = LocalTime::from_unix_timestamp(now - 3600).expect("start");
    let end = LocalTime::from_unix_timestamp(now + 3600).expect("end");
    state
        .db
        .records()
        .by_client_and_time_in(RecordTable::Live, uuid, start, end)
        .expect("read records")
}
