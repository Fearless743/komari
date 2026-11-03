mod common;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, StatusCode, header};
use common::{app, call, create_admin_session, create_client, default_config, memory_db, request};
use komari_api::{ApiState, client_router_with_state};
use komari_geoip::{GeoInfo, GeoIpProvider, GeoIpService, error::GeoIpError};
use serde_json::json;

fn uri(token: &str) -> String {
    format!("/api/clients/uploadBasicInfo?token={token}")
}

#[tokio::test]
async fn basic_info_updates_client_row() {
    let (state, app) = app();
    let client = create_client(&state, "old-name");
    let body = json!({
        "name": "new-name",
        "cpu_name": "Ryzen",
        "virtualization": "kvm",
        "arch": "x86_64",
        "cpu_cores": 8,
        "os": "Linux",
        "kernel_version": "6.1",
        "gpu_name": "None",
        "ipv4": "203.0.113.7",
        "region": "JP",
        "remark": "test",
        "mem_total": 16_000_000_000_i64,
        "swap_total": 2_000_000_000_i64,
        "disk_total": 500_000_000_000_i64,
        "version": "1.2.3",
        "weight": 7,
        "price": 3.5,
        "billing_cycle": 30,
        "currency": "USD",
        "tags": "a;b",
        "hidden": true,
    });

    let (status, response) = call(
        &app,
        request(
            Method::POST,
            &uri(&client.token),
            Body::from(body.to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response, json!({"status": "success"}));

    let updated = state.db.clients().require(&client.uuid).unwrap();
    assert_eq!(updated.name, "new-name");
    assert_eq!(updated.cpu_name, "Ryzen");
    assert_eq!(updated.virtualization, "kvm");
    assert_eq!(updated.cpu_cores, 8);
    assert_eq!(updated.os, "Linux");
    assert_eq!(updated.kernel_version, "6.1");
    assert_eq!(updated.gpu_name, "None");
    assert_eq!(updated.ipv4, "203.0.113.7");
    assert_eq!(updated.region, "JP");
    assert_eq!(updated.mem_total, 16_000_000_000);
    assert_eq!(updated.swap_total, 2_000_000_000);
    assert_eq!(updated.disk_total, 500_000_000_000);
    assert_eq!(updated.version, "1.2.3");
    assert_eq!(updated.weight, 7);
    assert!((updated.price - 3.5).abs() < f64::EPSILON);
    assert_eq!(updated.billing_cycle, 30);
    assert_eq!(updated.currency, "USD");
    assert_eq!(updated.tags, "a;b");
    assert!(updated.hidden);
}

#[tokio::test]
async fn basic_info_partial_update_preserves_other_columns() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");
    state
        .db
        .clients()
        .patch(
            &client.uuid,
            &komari_db::ClientPatch {
                cpu_name: Some("keep-me".to_owned()),
                remark: Some("keep remark".to_owned()),
                mem_total: Some(4096),
                ..komari_db::ClientPatch::default()
            },
        )
        .unwrap();

    let body = json!({"name": "renamed"});
    let (status, _) = call(
        &app,
        request(
            Method::POST,
            &uri(&client.token),
            Body::from(body.to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let updated = state.db.clients().require(&client.uuid).unwrap();
    assert_eq!(updated.name, "renamed");
    assert_eq!(updated.cpu_name, "keep-me");
    assert_eq!(updated.remark, "keep remark");
    assert_eq!(updated.mem_total, 4096);
}

#[tokio::test]
async fn basic_info_falls_back_to_trusted_proxy_headers() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");
    let app = app.layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 42], 1234))));

    let body = json!({"uuid": "ignored-body-uuid", "name": "ip-test"});
    let mut http_request = request(
        Method::POST,
        &uri(&client.token),
        Body::from(body.to_string()),
    );
    http_request.headers_mut().insert(
        header::HeaderName::from_static("x-forwarded-for"),
        header::HeaderValue::from_static("203.0.113.9, 10.0.0.1"),
    );
    let (status, _) = call(&app, http_request).await;
    assert_eq!(status, StatusCode::OK);

    let updated = state.db.clients().require(&client.uuid).unwrap();
    assert_eq!(updated.name, "ip-test");
    assert_eq!(updated.ipv4, "203.0.113.9");
    assert_eq!(updated.ipv6, "");
}

struct JapanProvider;

#[async_trait]
impl GeoIpProvider for JapanProvider {
    fn name(&self) -> &str {
        "test"
    }

    async fn get_geo_info(&self, _ip: IpAddr) -> Result<GeoInfo, GeoIpError> {
        Ok(GeoInfo::country("JP", "Japan"))
    }
}

#[tokio::test]
async fn basic_info_writes_geoip_region_emoji() {
    let db = memory_db();
    db.config().set("geo_ip_enabled", &true).unwrap();
    let state = ApiState::with_config(db, default_config());
    let geoip = Arc::new(GeoIpService::new());
    geoip.set_provider("test", Box::new(JapanProvider));
    let state = state.with_geoip_service(geoip);
    let app = client_router_with_state(state.clone());

    let client = create_client(&state, "geo");
    let body = json!({"ipv4": "203.0.113.7", "name": "geo-test"});
    let (status, _) = call(
        &app,
        request(
            Method::POST,
            &uri(&client.token),
            Body::from(body.to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let updated = state.db.clients().require(&client.uuid).unwrap();
    assert_eq!(updated.region, "🇯🇵");
}

#[tokio::test]
async fn basic_info_accepts_body_token_but_not_bearer_client_token() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");

    let body = json!({"name": "body-token", "token": client.token});
    let (status, _) = call(
        &app,
        request(
            Method::POST,
            "/api/clients/uploadBasicInfo",
            Body::from(body.to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        state.db.clients().require(&client.uuid).unwrap().name,
        "body-token"
    );

    let body = json!({"name": "bearer"});
    let (status, _) = call(
        &app,
        common::bearer_request(
            Method::POST,
            "/api/clients/uploadBasicInfo",
            &client.token,
            Body::from(body.to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn basic_info_admin_session_may_target_body_uuid() {
    let (state, app) = app();
    let target = create_client(&state, "target");
    let session = create_admin_session(&state);

    let body = json!({"uuid": target.uuid, "name": "admin-updated"});
    let mut admin_request = request(
        Method::POST,
        "/api/clients/uploadBasicInfo",
        Body::from(body.to_string()),
    );
    admin_request.headers_mut().insert(
        header::COOKIE,
        format!("session_token={session}").parse().unwrap(),
    );
    let (status, _) = call(&app, admin_request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        state.db.clients().require(&target.uuid).unwrap().name,
        "admin-updated"
    );
}

#[tokio::test]
async fn basic_info_rejects_invalid_json_and_tokens() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");

    let (status, response) = call(
        &app,
        request(Method::POST, &uri(&client.token), Body::from("{not json")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(response["status"], "error");
    assert_eq!(response["error"], "Invalid or missing data");

    let body = json!({"name": "x"}).to_string();
    let (status, response) = call(
        &app,
        request(Method::POST, &uri("bad-token"), Body::from(body.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(response["message"], "Unauthorized.");

    let (status, _) = call(
        &app,
        request(
            Method::POST,
            "/api/clients/uploadBasicInfo",
            Body::from(body),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        state.db.clients().require(&client.uuid).unwrap().name,
        "web-1"
    );
}

#[tokio::test]
async fn basic_info_rejects_invalid_numeric_fields() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");
    let body = json!({"cpu_cores": -1});

    let (status, response) = call(
        &app,
        request(
            Method::POST,
            &uri(&client.token),
            Body::from(body.to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(response["status"], "error");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("cpu_cores must be non-negative")
    );
}
