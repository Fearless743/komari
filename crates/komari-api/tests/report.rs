mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode, header};
use common::{
    app, bearer_request, call, create_admin_session, create_client, flush, gpu_rows_for,
    record_rows_for, records_for, report_body, request, set_api_key,
};
use komari_api::{ClientEventKind, PresenceSource};
use serde_json::json;

fn report_uri(token: &str) -> String {
    format!("/api/clients/report?token={token}")
}

fn post_report(uri: &str, body: &serde_json::Value) -> axum::http::Request<Body> {
    request(Method::POST, uri, Body::from(body.to_string()))
}

#[tokio::test]
async fn http_report_buffers_until_flush_then_writes_aggregate() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");
    let mut events = state.hub.subscribe();

    let body = report_body(&client.uuid);
    let (status, response) = call(&app, post_report(&report_uri(&client.token), &body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response, json!({"status": "success"}));

    // API layer only writes the in-memory cache.
    assert!(records_for(&state, &client.uuid).is_empty());
    assert_eq!(state.report_cache.buffered(&client.uuid), 1);

    let cached = state
        .hub
        .latest_report(&client.uuid)
        .expect("latest report");
    assert_eq!(cached.uuid, client.uuid);
    assert!((cached.cpu.usage - 12.5).abs() < f64::EPSILON);
    assert!(cached.updated_at.is_some());

    let summary = flush(&state);
    assert_eq!(summary.clients, 1);
    assert_eq!(summary.records, 1);
    // Recent samples stay buffered for the RPC layer.
    assert_eq!(state.report_cache.buffered(&client.uuid), 1);
    assert_eq!(state.report_cache.recent_reports()[&client.uuid].len(), 1);

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

    let body = json!({
        "cpu": {"usage": 1.0},
        "future_field": {"nested": [1, 2, 3]},
    });
    let (status, response) = call(&app, post_report(&report_uri(&client.token), &body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["status"], "success");

    flush(&state);
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
    assert!(records_for(&state, &client.uuid).is_empty());
}

#[tokio::test]
async fn bearer_client_token_is_a_guest_but_body_token_authenticates() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");
    let body = report_body(&client.uuid).to_string();

    // Go only treats Authorization as the configured admin API key.
    let (status, response) = call(
        &app,
        bearer_request(
            Method::POST,
            "/api/clients/report",
            &client.token,
            Body::from(body.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(response["message"], "Unauthorized.");

    // POST body tokens are accepted and the body remains readable.
    let mut body_value = report_body(&client.uuid);
    body_value["token"] = json!(client.token);
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
    flush(&state);
    assert_eq!(records_for(&state, &client.uuid).len(), 1);
}

#[tokio::test]
async fn http_report_accepts_large_bodies_like_go() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");
    let mut body = report_body(&client.uuid);
    body["padding"] = json!("x".repeat(2 * 1024 * 1024));

    let (status, _) = call(&app, post_report(&report_uri(&client.token), &body)).await;
    assert_eq!(status, StatusCode::OK);
    flush(&state);
    assert_eq!(records_for(&state, &client.uuid).len(), 1);
}

#[tokio::test]
async fn flush_rejects_invalid_metric_ranges() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");
    let mut body = report_body(&client.uuid);
    body["cpu"]["usage"] = json!(140.0);

    // Go accepts the report into its cache; validation failure discards only
    // that client's batch.
    let (status, _) = call(&app, post_report(&report_uri(&client.token), &body)).await;
    assert_eq!(status, StatusCode::OK);
    let summary = flush(&state);
    assert_eq!(summary.discarded_clients, 1);
    assert!(records_for(&state, &client.uuid).is_empty());
    assert_eq!(state.report_cache.buffered(&client.uuid), 0);
}

#[tokio::test]
async fn save_clamps_tiny_and_negative_cpu_usage_before_verify() {
    let (state, app) = app();
    let client = create_client(&state, "web-1");

    for usage in [0.005, -3.0] {
        let mut body = report_body(&client.uuid);
        body["cpu"]["usage"] = json!(usage);
        let (status, _) = call(&app, post_report(&report_uri(&client.token), &body)).await;
        assert_eq!(status, StatusCode::OK);
    }
    flush(&state);
    let records = records_for(&state, &client.uuid);
    assert_eq!(records.len(), 1);
    // Top 30% of two samples is the highest raw value (0.005), clamped to
    // 0.01 on the database path.
    assert!((records[0].cpu - 0.01).abs() < f32::EPSILON);
}

#[tokio::test]
async fn http_report_writes_gpu_rows() {
    let (state, app) = app();
    let client = create_client(&state, "gpu-node");
    let mut body = report_body(&client.uuid);
    body["gpu"] = json!({
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

    let (status, _) = call(&app, post_report(&report_uri(&client.token), &body)).await;
    assert_eq!(status, StatusCode::OK);
    flush(&state);

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

#[tokio::test]
async fn body_uuid_wins_over_client_token_and_admin_may_target_a_client() {
    let (state, app) = app();
    let token_client = create_client(&state, "agent");
    let target = create_client(&state, "target");

    let body = report_body(&target.uuid);
    let (status, _) = call(&app, post_report(&report_uri(&token_client.token), &body)).await;
    assert_eq!(status, StatusCode::OK);
    flush(&state);
    assert!(records_for(&state, &token_client.uuid).is_empty());
    assert_eq!(records_for(&state, &target.uuid).len(), 1);

    // Admin session with a body UUID (no agent token at all).
    let session = create_admin_session(&state);
    let body = report_body(&target.uuid);
    let mut admin_request = post_report("/api/clients/report", &body);
    admin_request.headers_mut().insert(
        header::COOKIE,
        format!("session_token={session}").parse().unwrap(),
    );
    let (status, _) = call(&app, admin_request).await;
    assert_eq!(status, StatusCode::OK);
    flush(&state);
    assert_eq!(records_for(&state, &target.uuid).len(), 2);

    // Admin API key works too.
    set_api_key(&state, "0123456789abcdef");
    let body = report_body(&target.uuid);
    let mut api_key_request = post_report("/api/clients/report", &body);
    api_key_request.headers_mut().insert(
        header::AUTHORIZATION,
        header::HeaderValue::from_static("Bearer 0123456789abcdef"),
    );
    let (status, _) = call(&app, api_key_request).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn one_invalid_client_does_not_block_other_flushes() {
    let (state, app) = app();
    let mut normal = Vec::new();
    for index in 0..10 {
        normal.push(create_client(&state, &format!("normal-{index}")));
    }
    let invalid = create_client(&state, "invalid");

    for client in &normal {
        let (status, _) = call(
            &app,
            post_report(&report_uri(&client.token), &report_body(&client.uuid)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let mut bad_body = report_body(&invalid.uuid);
    bad_body["cpu"]["usage"] = json!(140.0);
    let (status, _) = call(&app, post_report(&report_uri(&invalid.token), &bad_body)).await;
    assert_eq!(status, StatusCode::OK);

    let summary = flush(&state);
    assert_eq!(summary.clients, 10);
    assert_eq!(summary.records, 10);
    assert_eq!(summary.discarded_clients, 1);
    assert_eq!(summary.failed_clients, 0);
    for client in normal {
        assert_eq!(records_for(&state, &client.uuid).len(), 1);
    }
    assert!(records_for(&state, &invalid.uuid).is_empty());
}

#[tokio::test]
async fn cache_clamps_each_sample_before_aggregation() {
    let (state, _app) = app();
    let client = create_client(&state, "mixed");
    let usages = [20.0, 15.0, -3.0, -4.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    for usage in usages {
        state.report_cache.record(
            &client.uuid,
            komari_api::Report {
                cpu: komari_api::CpuReport {
                    usage,
                    ..komari_api::CpuReport::default()
                },
                ..komari_api::Report::default()
            },
        );
    }
    flush(&state);
    let records = records_for(&state, &client.uuid);
    assert_eq!(records.len(), 1);
    // (20 + 15 + 0.01) / 3 == 11.67, matching Go's per-sample clamp.
    assert!((records[0].cpu - 11.67).abs() < 0.01);
}

#[tokio::test]
async fn gpu_aggregation_uses_top_thirty_percent() {
    let (state, _app) = app();
    let client = create_client(&state, "gpu");
    for utilization in [10.0, 20.0, 90.0] {
        state.report_cache.record(
            &client.uuid,
            komari_api::Report {
                gpu: Some(komari_api::GpuDetailReport {
                    count: 1,
                    average_usage: utilization,
                    detailed_info: vec![komari_api::GpuDeviceInfo {
                        name: "gpu0".to_owned(),
                        memory_total: 1024,
                        memory_used: 512,
                        utilization,
                        temperature: 50,
                    }],
                }),
                ..komari_api::Report::default()
            },
        );
    }
    flush(&state);
    let records = records_for(&state, &client.uuid);
    assert_eq!(records.len(), 1);
    let gpu = state
        .db
        .records()
        .gpu_by_client_and_time_in(
            komari_db::RecordTable::Live,
            &client.uuid,
            records[0].time,
            records[0].time,
        )
        .unwrap();
    assert_eq!(gpu.len(), 1);
    assert!((gpu[0].utilization - 90.0).abs() < f32::EPSILON);
}

#[test]
fn database_errors_keep_samples_buffered_for_retry() {
    use komari_api::{ManualClock, ReportCache};
    use std::sync::Arc;
    use std::time::Duration;

    let db = common::memory_db();
    db.execute_batch("DROP TABLE records").unwrap();
    let cache = ReportCache::new(
        Arc::new(ManualClock::new(1_000_000)),
        Duration::from_secs(60),
        0.3,
    );
    cache.record(
        "client",
        komari_api::Report {
            cpu: komari_api::CpuReport {
                usage: 10.0,
                ..komari_api::CpuReport::default()
            },
            ..komari_api::Report::default()
        },
    );
    let (summary, _error) = cache.flush(&db).expect_err("flush must fail");
    assert_eq!(summary.failed_clients, 1);
    assert_eq!(cache.buffered("client"), 1);
}

#[test]
fn record_failure_rolls_back_gpu_rows_and_retries_once() {
    use komari_api::{ManualClock, ReportCache};
    use std::sync::Arc;
    use std::time::Duration;

    let db = common::memory_db();
    db.execute_batch("DROP TABLE records").unwrap();
    let cache = ReportCache::new(
        Arc::new(ManualClock::new(1_000_000)),
        Duration::from_secs(60),
        0.3,
    );
    cache.record(
        "gpu-client",
        komari_api::Report {
            gpu: Some(komari_api::GpuDetailReport {
                count: 1,
                average_usage: 50.0,
                detailed_info: vec![komari_api::GpuDeviceInfo {
                    name: "gpu0".to_owned(),
                    memory_total: 1024,
                    memory_used: 512,
                    utilization: 50.0,
                    temperature: 50,
                }],
            }),
            ..komari_api::Report::default()
        },
    );

    assert!(cache.flush(&db).is_err());
    assert!(cache.flush(&db).is_err());
    assert_eq!(gpu_rows_for(&db, "gpu-client"), 0);
    assert_eq!(cache.buffered("gpu-client"), 1);
}

#[test]
fn failed_client_keeps_gpu_pending_while_others_commit() {
    use komari_api::{ManualClock, ReportCache};
    use std::sync::Arc;
    use std::time::Duration;

    let db = common::memory_db();
    db.execute_batch(
        "CREATE TRIGGER fail_bad BEFORE INSERT ON records          WHEN NEW.client = 'bad' BEGIN SELECT RAISE(ABORT, 'boom'); END;",
    )
    .unwrap();
    let cache = ReportCache::new(
        Arc::new(ManualClock::new(1_000_000)),
        Duration::from_secs(60),
        0.3,
    );
    cache.record(
        "bad",
        komari_api::Report {
            gpu: Some(komari_api::GpuDetailReport {
                count: 1,
                average_usage: 50.0,
                detailed_info: vec![komari_api::GpuDeviceInfo {
                    name: "gpu0".to_owned(),
                    memory_total: 1024,
                    memory_used: 512,
                    utilization: 50.0,
                    temperature: 50,
                }],
            }),
            ..komari_api::Report::default()
        },
    );
    cache.record(
        "good",
        komari_api::Report {
            cpu: komari_api::CpuReport {
                usage: 10.0,
                ..komari_api::CpuReport::default()
            },
            ..komari_api::Report::default()
        },
    );

    let (summary, error) = cache.flush(&db).expect_err("bad client must fail");
    assert_eq!(summary.clients, 1);
    assert_eq!(summary.records, 1);
    assert_eq!(summary.failed_clients, 1);
    assert!(error.contains("bad"));
    assert_eq!(record_rows_for(&db, "good"), 1);
    assert_eq!(record_rows_for(&db, "bad"), 0);
    assert_eq!(gpu_rows_for(&db, "bad"), 0);
    assert_eq!(cache.buffered("bad"), 1);

    db.execute_batch("DROP TRIGGER fail_bad;").unwrap();
    let summary = cache.flush(&db).expect("retry succeeds");
    assert_eq!(summary.clients, 2);
    assert_eq!(record_rows_for(&db, "bad"), 1);
    assert_eq!(gpu_rows_for(&db, "bad"), 1);
}

#[tokio::test]
async fn latest_report_keeps_raw_tiny_cpu_value() {
    let (state, app) = app();
    let client = create_client(&state, "raw");
    let mut body = report_body(&client.uuid);
    body["cpu"]["usage"] = json!(0.005);
    let (status, _) = call(&app, post_report(&report_uri(&client.token), &body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        state.hub.latest_report(&client.uuid).unwrap().cpu.usage,
        0.005
    );
}
