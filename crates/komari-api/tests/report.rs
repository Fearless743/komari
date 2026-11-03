mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use common::{
    app, app_with, bearer_request, call, create_client, default_config, records_for, report_body,
    request,
};
use komari_api::{ApiConfig, ClientEventKind, PresenceSource};

fn report_uri(token: &str) -> String {
    format!("/api/clients/report?token={token}")
}

#[tokio::test]
async fn http_report_persists_record_and_marks_online() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");
    let mut events = state.hub.subscribe();

    let body = report_body(&client.uuid);
    let (status, response) = call(
        &app,
        request(
            Method::POST,
            &report_uri(&client.token),
            Body::from(body.to_string()),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(response, serde_json::json!({"status": "success"}));

    let records = records_for(&state, &client.uuid);
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.client, client.uuid);
    assert!((record.cpu - 12.5).abs() < f32::EPSILON);
    assert_eq!(record.ram, 1024);
    assert_eq!(record.ram_total, 2048);
    assert_eq!(record.disk, 100);
    assert_eq!(record.disk_total, 200);
    assert_eq!(record.net_in, 22);
    assert_eq!(record.net_out, 11);
    assert_eq!(record.net_total_up, 33);
    assert_eq!(record.net_total_down, 44);
    assert_eq!(record.process, 5);
    assert_eq!(record.connections, 3);
    assert_eq!(record.connections_udp, 4);

    let cached = state
        .hub
        .latest_report(&client.uuid)
        .expect("cached report");
    assert_eq!(cached.uuid, client.uuid);
    assert!(state.presence.connection_id(&client.uuid).is_some());
    assert!(state.hub.is_online(&client.uuid));

    let event = events.try_recv().expect("online event");
    assert_eq!(event.kind, ClientEventKind::Online);
    assert_eq!(event.source, PresenceSource::HttpPost);
    assert_eq!(event.uuid, client.uuid);
}

#[tokio::test]
async fn http_report_accepts_missing_sections_and_unknown_fields() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");

    let body = serde_json::json!({
        "cpu": {"usage": 1.0},
        "future_field": {"nested": [1, 2, 3]},
    });
    let (status, response) = call(
        &app,
        request(
            Method::POST,
            &report_uri(&client.token),
            Body::from(body.to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["status"], "success");

    let records = records_for(&state, &client.uuid);
    assert_eq!(records.len(), 1);
    assert!((records[0].cpu - 1.0).abs() < f32::EPSILON);
    assert_eq!(records[0].ram, 0);
    assert_eq!(records[0].net_in, 0);
}

#[tokio::test]
async fn http_report_rejects_invalid_json_and_bad_tokens() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");

    let (status, response) = call(
        &app,
        request(
            Method::POST,
            &report_uri(&client.token),
            Body::from("{not json"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(response["error"], "Invalid request body");

    let body = report_body(&client.uuid).to_string();
    let (status, response) = call(
        &app,
        request(
            Method::POST,
            &report_uri("wrong-token"),
            Body::from(body.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(response["status"], "error");
    assert_eq!(response["message"], "Unauthorized.");

    let (status, _) = call(
        &app,
        request(Method::POST, "/api/clients/report", Body::from(body)),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(records_for(&state, &client.uuid).len(), 0);
}

#[tokio::test]
async fn http_report_accepts_bearer_header_and_body_token() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");
    let body = report_body(&client.uuid).to_string();

    let (status, _) = call(
        &app,
        bearer_request(
            Method::POST,
            "/api/clients/report",
            &client.token,
            Body::from(body.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let mut body_value = report_body(&client.uuid);
    body_value["token"] = serde_json::Value::String(client.token.clone());
    let (status, _) = call(
        &app,
        request(
            Method::POST,
            "/api/clients/report",
            Body::from(body_value.to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(records_for(&state, &client.uuid).len(), 2);
}

#[tokio::test]
async fn http_report_rejects_oversized_body() {
    let config = ApiConfig {
        max_report_body: 32,
        ..default_config()
    };
    let (state, app) = app_with(config);
    let client = create_client(&state, "web-1");
    let body = report_body(&client.uuid).to_string();
    assert!(body.len() > 32);

    let (status, response) = call(
        &app,
        request(Method::POST, &report_uri(&client.token), Body::from(body)),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(response["error"], "Invalid request body");
    assert_eq!(records_for(&state, &client.uuid).len(), 0);
}

#[tokio::test]
async fn http_report_rejects_invalid_metric_ranges() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");
    let mut body = report_body(&client.uuid);
    body["cpu"]["usage"] = serde_json::json!(140.0);

    let (status, response) = call(
        &app,
        request(
            Method::POST,
            &report_uri(&client.token),
            Body::from(body.to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("CPU.Usage must be between 0 and 100")
    );
    assert_eq!(records_for(&state, &client.uuid).len(), 0);
}

#[tokio::test]
async fn http_report_writes_gpu_rows() {
    let (state, app) = app();
    let client = create_client(&state, "gpu-node");
    let mut body = report_body(&client.uuid);
    body["gpu"] = serde_json::json!({
        "count": 1,
        "average_usage": 50.0,
        "detailed_info": [{
            "name": "Test GPU",
            "memory_total": 1024,
            "memory_used": 512,
            "utilization": 50.0,
            "temperature": 60,
        }],
    });

    let (status, _) = call(
        &app,
        request(
            Method::POST,
            &report_uri(&client.token),
            Body::from(body.to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let records = records_for(&state, &client.uuid);
    assert_eq!(records.len(), 1);
    assert!((records[0].gpu - 50.0).abs() < f32::EPSILON);

    let gpu = state
        .db
        .records()
        .gpu_by_client_and_time_in(
            komari_db::RecordTable::Live,
            &client.uuid,
            records[0].time,
            records[0].time,
        )
        .expect("gpu rows");
    assert_eq!(gpu.len(), 1);
    assert_eq!(gpu[0].device_name, "Test GPU");
    assert_eq!(gpu[0].mem_used, 512);
    assert_eq!(gpu[0].temperature, 60);
}
