//! `POST /api/clients/uploadBasicInfo` — static hardware/OS information.

use std::net::{IpAddr, SocketAddr};

use axum::body::to_bytes;
use axum::extract::connect_info::MockConnectInfo;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use komari_config::keys;
use komari_db::{ClientPatch, LocalTime};
use komari_geoip::region_unicode_emoji;
use serde_json::{Map, Value, json};

use super::unauthorized;
use crate::response::status_success;
use crate::state::ApiState;

pub async fn upload_basic_info(State(state): State<ApiState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0)
        .or_else(|| {
            parts
                .extensions
                .get::<MockConnectInfo<SocketAddr>>()
                .map(|connect_info| connect_info.0)
        });
    let body = match to_bytes(body, usize::MAX).await {
        Ok(body) => body,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                basic_info_error("Invalid or missing data"),
            )
                .into_response();
        }
    };

    let context = komari_auth::authenticate_request_with(
        &state.auth,
        &parts.headers,
        &parts.uri,
        &parts.method,
        Some(&body),
    )
    .await;
    if !context.permits(&[komari_auth::Role::Admin, komari_auth::Role::Client]) {
        return unauthorized();
    }

    let value = match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(map)) => map,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                basic_info_error("Invalid or missing data"),
            )
                .into_response();
        }
    };

    let mut info = value;
    // Agent identities always update their own row. Administrators may target
    // a client explicitly through the body UUID.
    let uuid = context.client_uuid.clone().or_else(|| {
        info.get("uuid")
            .and_then(Value::as_str)
            .filter(|uuid| !uuid.is_empty())
            .map(str::to_owned)
    });
    let Some(uuid) = uuid else {
        return (StatusCode::BAD_REQUEST, basic_info_error("Invalid token")).into_response();
    };
    info.insert("uuid".to_owned(), Value::String(uuid.clone()));

    // If the agent did not provide an address, use the same trusted proxy
    // headers as Go's `c.ClientIP()` and fall back to the socket peer.
    let has_address = non_empty_string(&info, "ipv4") || non_empty_string(&info, "ipv6");
    if !has_address {
        if let Some(ip) = trusted_client_ip(&parts.headers, peer.map(|address| address.ip())) {
            let key = if ip.is_ipv4() { "ipv4" } else { "ipv6" };
            info.insert(key.to_owned(), Value::String(ip.to_string()));
        }
    }

    // Go writes the regional-indicator emoji when GeoIP is enabled.
    if let Ok(Some(true)) = state.db.config().get_as::<bool>(keys::GEO_IP_ENABLED_KEY) {
        if let Some(service) = state.geoip.as_ref() {
            let address = non_empty_string_value(&info, "ipv4")
                .or_else(|| non_empty_string_value(&info, "ipv6"))
                .and_then(|value| value.parse::<IpAddr>().ok());
            if let Some(address) = address {
                let geo = service.lookup(address).await;
                if !geo.is_empty() {
                    info.insert(
                        "region".to_owned(),
                        Value::String(region_unicode_emoji(&geo.iso_code)),
                    );
                }
            }
        }
    }

    let patch = match basic_info_patch(&info) {
        Ok(patch) => patch,
        Err(message) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, basic_info_error(message)).into_response();
        }
    };

    match state.db.clients().patch(&uuid, &patch) {
        Ok(_) => (StatusCode::OK, status_success()).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            basic_info_error(error.to_string()),
        )
            .into_response(),
    }
}

fn basic_info_error(message: impl Into<String>) -> axum::Json<Value> {
    axum::Json(json!({"status": "error", "error": message.into()}))
}

fn non_empty_string(map: &Map<String, Value>, key: &str) -> bool {
    non_empty_string_value(map, key).is_some()
}

fn non_empty_string_value(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn trusted_client_ip(headers: &axum::http::HeaderMap, peer: Option<IpAddr>) -> Option<IpAddr> {
    if let Some(value) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
    {
        if let Ok(ip) = value.trim().parse::<IpAddr>() {
            return Some(ip);
        }
    }
    if let Some(value) = headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<IpAddr>().ok())
    {
        return Some(value);
    }
    peer
}

fn basic_info_patch(info: &Map<String, Value>) -> Result<ClientPatch, String> {
    let expired_at = match info.get("expired_at").filter(|value| !value.is_null()) {
        Some(value) => {
            let text = value
                .as_str()
                .ok_or_else(|| "expired_at must be a timestamp string".to_owned())?;
            Some(LocalTime::parse(text).map_err(|error| format!("invalid expired_at: {error}"))?)
        }
        None => None,
    };

    Ok(ClientPatch {
        name: string_field(info, "name")?,
        cpu_name: string_field(info, "cpu_name")?,
        virtualization: string_field(info, "virtualization")?,
        arch: string_field(info, "arch")?,
        cpu_cores: non_negative_int(info, "cpu_cores")?,
        os: string_field(info, "os")?,
        kernel_version: string_field(info, "kernel_version")?,
        gpu_name: string_field(info, "gpu_name")?,
        ipv4: string_field(info, "ipv4")?,
        ipv6: string_field(info, "ipv6")?,
        region: string_field(info, "region")?,
        remark: string_field(info, "remark")?,
        public_remark: string_field(info, "public_remark")?,
        mem_total: non_negative_int(info, "mem_total")?,
        swap_total: non_negative_int(info, "swap_total")?,
        disk_total: non_negative_int(info, "disk_total")?,
        version: string_field(info, "version")?,
        weight: int_field(info, "weight")?,
        price: float_field(info, "price")?,
        billing_cycle: int_field(info, "billing_cycle")?,
        auto_renewal: bool_field(info, "auto_renewal")?,
        currency: string_field(info, "currency")?,
        expired_at,
        group: string_field(info, "group")?,
        tags: string_field(info, "tags")?,
        ddns_enabled: bool_field(info, "ddns_enabled")?,
        ddns_hostname: string_field(info, "ddns_hostname")?,
        ddns_record_id: string_field(info, "ddns_record_id")?,
        ddns_record_type: string_field(info, "ddns_record_type")?,
        hidden: bool_field(info, "hidden")?,
        traffic_limit: non_negative_int(info, "traffic_limit")?,
        traffic_limit_type: string_field(info, "traffic_limit_type")?,
        ..ClientPatch::default()
    })
}

fn string_field(info: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match info.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn int_field(info: &Map<String, Value>, key: &str) -> Result<Option<i64>, String> {
    match info.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value_as_i64(value)
            .map(Some)
            .map_err(|()| format!("{key} must be an integer")),
    }
}

fn non_negative_int(info: &Map<String, Value>, key: &str) -> Result<Option<i64>, String> {
    let value = int_field(info, key)?;
    if let Some(value) = value {
        if value < 0 {
            return Err(format!("{key} must be non-negative, got {value}"));
        }
    }
    Ok(value)
}

fn float_field(info: &Map<String, Value>, key: &str) -> Result<Option<f64>, String> {
    match info.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| format!("{key} must be a number")),
    }
}

fn bool_field(info: &Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    match info.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("{key} must be a boolean")),
    }
}

fn value_as_i64(value: &Value) -> Result<i64, ()> {
    if let Some(value) = value.as_i64() {
        return Ok(value);
    }
    if let Some(value) = value.as_u64() {
        return i64::try_from(value).map_err(|_| ());
    }
    if let Some(value) = value.as_f64() {
        if value.is_finite() && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
            return Ok(value.trunc() as i64);
        }
    }
    Err(())
}
