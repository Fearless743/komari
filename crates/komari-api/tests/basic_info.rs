mod common;

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, StatusCode};
use common::{app, call, create_client, request};
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
async fn basic_info_falls_back_to_peer_address() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");
    let app = app.layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 42], 1234))));

    let body = json!({"uuid": "ignored-body-uuid", "name": "ip-test"});
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
    assert_eq!(updated.name, "ip-test");
    assert_eq!(updated.ipv4, "10.0.0.42");
    assert_eq!(updated.ipv6, "");
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
