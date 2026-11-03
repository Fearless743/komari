//! `/api/recent/{uuid}` and `/api/records/load`.
//!
//! Ports of Go `api/public/latestRecord.go` and `api/public/record.go`.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use komari_db::LocalTime;
use komari_db::models::{GPURecord, Record};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::response;
use crate::state::ApiState;

use super::{respond_error, respond_success, session_login};

/// Largest whole-hour offset that still fits in Go's `time.Duration`.
///
/// `time.Duration` is an `int64` nanosecond count; Go silently wraps larger
/// values. The Rust port rejects them with 400 instead (registered in the
/// crate README).
const MAX_HOURS: u64 = 2_562_047;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RecordsQuery {
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    load_type: Option<String>,
    #[serde(default)]
    hours: Option<String>,
}

pub(crate) async fn get_recent(
    State(state): State<ApiState>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
) -> Response {
    if hidden_for_guest(&state, &uuid, session_login(&state, &headers)) {
        // Go reuses the "UUID is required" message to hide the client.
        return respond_error(StatusCode::BAD_REQUEST, "UUID is required");
    }

    match state.report_cache.recent_for(&uuid) {
        Some(records) => {
            let data = serde_json::to_value(&records).unwrap_or(Value::Null);
            respond_success(data)
        }
        // Go's `cache.Get` returns a nil interface; `data,omitempty` then drops
        // the key entirely.
        None => (StatusCode::OK, response::success_optional(None)).into_response(),
    }
}

pub(crate) async fn get_records(
    State(state): State<ApiState>,
    Query(query): Query<RecordsQuery>,
    headers: HeaderMap,
) -> Response {
    let uuid = query.uuid.unwrap_or_default();
    let load_type = query.load_type.unwrap_or_default();

    if hidden_for_guest(&state, &uuid, session_login(&state, &headers)) {
        return respond_error(StatusCode::BAD_REQUEST, "UUID is required");
    }
    if uuid.is_empty() {
        return respond_error(StatusCode::BAD_REQUEST, "UUID is required");
    }

    let hours = query
        .hours
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "4".to_owned());
    let hours: i64 = match hours.parse() {
        Ok(hours) => hours,
        Err(_) => return respond_error(StatusCode::BAD_REQUEST, "Invalid hours parameter"),
    };
    if hours.unsigned_abs() > MAX_HOURS {
        return respond_error(StatusCode::BAD_REQUEST, "Invalid hours parameter");
    }

    if !valid_load_type(&load_type) {
        return respond_error(StatusCode::BAD_REQUEST, "Invalid load_type parameter");
    }

    let Some(window) = chrono::TimeDelta::try_hours(hours) else {
        return respond_error(StatusCode::BAD_REQUEST, "Invalid hours parameter");
    };
    let Some(start_instant) = chrono::Utc::now().checked_sub_signed(window) else {
        return respond_error(StatusCode::BAD_REQUEST, "Invalid hours parameter");
    };
    let end = LocalTime::now();
    let start = LocalTime::from_instant(start_instant);
    let client_records = match state.db.records().by_client_and_time(&uuid, start, end) {
        Ok(records) => records,
        Err(error) => {
            return respond_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to fetch records: {error}"),
            );
        }
    };

    let mut response = Map::new();
    if load_type.is_empty() || load_type == "all" {
        // GORM returns a nil slice when no rows match, which Go encodes as
        // `null`; the filtered path allocates an empty slice (`[]`).
        let records = if client_records.is_empty() {
            Value::Null
        } else {
            Value::Array(client_records.iter().map(record_json).collect())
        };
        response.insert("records".to_owned(), records);
        response.insert("count".to_owned(), json!(client_records.len()));
    } else {
        let filtered = filter_records_by_load_type(&client_records, &load_type);
        let count = filtered.len();
        response.insert("records".to_owned(), Value::Array(filtered));
        response.insert("count".to_owned(), json!(count));
        response.insert("load_type".to_owned(), json!(load_type));
    }

    // `gpu` is no longer a valid `load_type` (Go's `validLoadTypes` omits it),
    // but the branch is kept for parity with the Go condition.
    if load_type.is_empty() || load_type == "all" || load_type == "gpu" {
        match state.db.records().gpu_by_client_and_time(&uuid, start, end) {
            Ok(gpu_records) if !gpu_records.is_empty() => {
                let mut devices: BTreeMap<String, Value> = BTreeMap::new();
                for record in gpu_records {
                    let key = record.device_index.to_string();
                    let entry = devices.entry(key).or_insert_with(|| {
                        json!({
                            "device_index": record.device_index,
                            "device_name": &record.device_name,
                            "records": [],
                        })
                    });
                    if let Some(records) = entry.get_mut("records").and_then(Value::as_array_mut) {
                        records.push(gpu_record_json(&record));
                    }
                }
                response.insert("gpu_devices".to_owned(), json!(devices));
                response.insert("has_gpu_data".to_owned(), json!(true));
            }
            _ => {
                response.insert("has_gpu_data".to_owned(), json!(false));
            }
        }
    }

    respond_success(Value::Object(response))
}

fn valid_load_type(load_type: &str) -> bool {
    matches!(
        load_type,
        "" | "cpu"
            | "ram"
            | "swap"
            | "load"
            | "temp"
            | "disk"
            | "network"
            | "process"
            | "connections"
            | "all"
    )
}

fn hidden_for_guest(state: &ApiState, uuid: &str, is_login: bool) -> bool {
    if is_login {
        return false;
    }
    // Go ignores database errors here and treats the hidden set as empty.
    state
        .db
        .clients()
        .list()
        .map(|clients| {
            clients
                .iter()
                .any(|client| client.hidden && client.uuid == uuid)
        })
        .unwrap_or(false)
}

/// Serialize a record through its JSON text form so `f32` fields use Rust's
/// shortest round-trip representation (`12.3`, not `12.300000190734863`),
/// matching Go's `encoding/json` output for `float32`.
fn record_json(record: &Record) -> Value {
    json_roundtrip(record)
}

fn gpu_record_json(record: &GPURecord) -> Value {
    json_roundtrip(record)
}

fn json_roundtrip<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_string(value)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// Convert an `f32` to a JSON number with its shortest decimal form.
fn f32_value(value: f32) -> Value {
    serde_json::from_str(&value.to_string()).unwrap_or(Value::Null)
}

/// Port of Go `filterRecordsByLoadType`.
fn filter_records_by_load_type(records: &[Record], load_type: &str) -> Vec<Value> {
    records
        .iter()
        .map(|record| {
            let mut filtered = Map::new();
            filtered.insert("client".to_owned(), json!(&record.client));
            filtered.insert("time".to_owned(), json!(record.time));

            match load_type {
                "cpu" => {
                    filtered.insert("cpu".to_owned(), f32_value(record.cpu));
                }
                "gpu" => {
                    filtered.insert("gpu".to_owned(), f32_value(record.gpu));
                }
                "ram" => {
                    filtered.insert("ram".to_owned(), json!(record.ram));
                    filtered.insert("ram_total".to_owned(), json!(record.ram_total));
                    if record.ram_total > 0 {
                        filtered.insert(
                            "ram_percent".to_owned(),
                            f32_value(record.ram as f32 / record.ram_total as f32 * 100.0),
                        );
                    }
                }
                "swap" => {
                    filtered.insert("swap".to_owned(), json!(record.swap));
                    filtered.insert("swap_total".to_owned(), json!(record.swap_total));
                    if record.swap_total > 0 {
                        filtered.insert(
                            "swap_percent".to_owned(),
                            f32_value(record.swap as f32 / record.swap_total as f32 * 100.0),
                        );
                    }
                }
                "load" => {
                    filtered.insert("load".to_owned(), f32_value(record.load));
                }
                "temp" => {
                    filtered.insert("temp".to_owned(), f32_value(record.temp));
                }
                "disk" => {
                    filtered.insert("disk".to_owned(), json!(record.disk));
                    filtered.insert("disk_total".to_owned(), json!(record.disk_total));
                    if record.disk_total > 0 {
                        filtered.insert(
                            "disk_percent".to_owned(),
                            f32_value(record.disk as f32 / record.disk_total as f32 * 100.0),
                        );
                    }
                }
                "network" => {
                    filtered.insert("net_in".to_owned(), json!(record.net_in));
                    filtered.insert("net_out".to_owned(), json!(record.net_out));
                    filtered.insert("net_total_up".to_owned(), json!(record.net_total_up));
                    filtered.insert("net_total_down".to_owned(), json!(record.net_total_down));
                }

                "process" => {
                    filtered.insert("process".to_owned(), json!(record.process));
                }
                "connections" => {
                    filtered.insert("connections".to_owned(), json!(record.connections));
                    filtered.insert("connections_udp".to_owned(), json!(record.connections_udp));
                    // Go's `int` subtraction wraps on overflow; avoid a Rust
                    // debug-build panic on corrupt database values.
                    filtered.insert(
                        "connections_tcp".to_owned(),
                        json!(record.connections.wrapping_sub(record.connections_udp)),
                    );
                }
                _ => {}
            }

            Value::Object(filtered)
        })
        .collect()
}
