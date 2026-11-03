//! Common RPC methods (`api/jsonRpc/common*.go`).

use std::collections::{HashMap, HashSet};

use komari_db::{Db, LocalTime};
use serde_json::{Value, json};

use super::context::{Permission, RpcContext};
use super::protocol::{INTERNAL_ERROR, INVALID_PARAMS, RpcError, RpcRequest};

/// Backend version, matching `utils.CurrentVersion` / `utils.VersionHash`.
pub const VERSION: &str = match option_env!("KOMARI_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
pub const VERSION_HASH: &str = match option_env!("KOMARI_VERSION_HASH") {
    Some(hash) => hash,
    None => "unknown",
};

// ---------------------------------------------------------------------------
// getNodes
// ---------------------------------------------------------------------------

#[derive(Default, serde::Deserialize)]
struct NodesParams {
    #[serde(default)]
    uuid: String,
}

pub(crate) fn get_nodes(context: &RpcContext, request: &RpcRequest) -> Result<Value, RpcError> {
    let params: NodesParams = request.bind_params();
    let mut clients = context.db.clients().list().map_err(|error| {
        RpcError::new(
            INTERNAL_ERROR,
            "Failed to get client info",
            Some(json!(error.to_string())),
        )
    })?;

    let send_ip = context
        .db
        .config()
        .get_as::<bool>(komari_config::keys::SEND_IP_ADDR_TO_GUEST_KEY)
        .ok()
        .flatten()
        .unwrap_or(false);

    if context.permission != Permission::Admin {
        clients = clients
            .into_iter()
            .filter(|client| !client.hidden)
            .map(|mut client| {
                if send_ip {
                    client.ipv4 = mask_ipv4(&client.ipv4);
                    client.ipv6 = mask_ipv6(&client.ipv6);
                } else {
                    client.ipv4.clear();
                    client.ipv6.clear();
                }
                client.remark.clear();
                client.version.clear();
                client.token.clear();
                client
            })
            .collect();
    }

    if !params.uuid.is_empty() {
        let Some(client) = clients.iter().find(|client| client.uuid == params.uuid) else {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "Node not found",
                Some(Value::String(params.uuid)),
            ));
        };
        return serde_json::to_value(client)
            .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string(), None));
    }

    let mut nodes = serde_json::Map::with_capacity(clients.len());
    for client in clients {
        let value = serde_json::to_value(&client)
            .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string(), None))?;
        nodes.insert(client.uuid.clone(), value);
    }
    Ok(Value::Object(nodes))
}

fn mask_ipv4(ip: &str) -> String {
    if ip.is_empty() {
        return String::new();
    }
    format!("{}.*.*.*", ip.split('.').next().unwrap_or(""))
}

fn mask_ipv6(ip: &str) -> String {
    if ip.is_empty() {
        return String::new();
    }
    format!("{}:*:*:*:*:*:*:*", ip.split(':').next().unwrap_or(""))
}

// ---------------------------------------------------------------------------
// getNodesLatestStatus
// ---------------------------------------------------------------------------

#[derive(Default, serde::Deserialize)]
struct LatestParams {
    #[serde(default)]
    uuid: String,
    #[serde(default)]
    uuids: Vec<String>,
}

pub(crate) fn get_nodes_latest_status(
    context: &RpcContext,
    request: &RpcRequest,
) -> Result<Value, RpcError> {
    let params: LatestParams = request.bind_params();
    let mut latest = context.hub.latest_reports();
    let online = context
        .hub
        .online_uuids()
        .into_iter()
        .collect::<HashSet<_>>();

    if context.permission != Permission::Admin {
        let hidden = hidden_clients(&context.db)?;
        latest.retain(|uuid, _| !hidden.contains(uuid));
    }

    if !params.uuid.is_empty() && !latest.contains_key(&params.uuid) {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "Node not found",
            Some(Value::String(params.uuid)),
        ));
    }

    let build = |uuid: &str, report: &crate::Report| {
        json!({
            "client": uuid,
            "time": report_time(report),
            "cpu": report.cpu.usage,
            "gpu": 0,
            "ram": report.ram.used,
            "ram_total": report.ram.total,
            "swap": report.swap.used,
            "swap_total": report.swap.total,
            "load": report.load.load1,
            "load5": report.load.load5,
            "load15": report.load.load15,
            "temp": 0,
            "disk": report.disk.used,
            "disk_total": report.disk.total,
            "net_in": report.network.down,
            "net_out": report.network.up,
            "net_total_up": report.network.total_up,
            "net_total_down": report.network.total_down,
            "process": report.process,
            "connections": report.connections.tcp + report.connections.udp,
            "connections_udp": report.connections.udp,
            "online": online.contains(uuid),
            "uptime": report.uptime,
        })
    };

    if !params.uuid.is_empty() {
        let report = latest
            .get(&params.uuid)
            .expect("uuid presence checked above");
        return Ok(build(&params.uuid, report));
    }

    let selected: Option<HashSet<&str>> = if params.uuids.is_empty() {
        None
    } else {
        Some(params.uuids.iter().map(String::as_str).collect())
    };
    let mut result = serde_json::Map::with_capacity(latest.len());
    for (uuid, report) in &latest {
        if selected
            .as_ref()
            .is_some_and(|selected| !selected.contains(uuid.as_str()))
        {
            continue;
        }
        result.insert(uuid.clone(), build(uuid, report));
    }
    Ok(Value::Object(result))
}

fn report_time(report: &crate::Report) -> LocalTime {
    report
        .updated_at
        .as_deref()
        .and_then(|value| LocalTime::parse(value).ok())
        .unwrap_or_else(LocalTime::zero)
}

// ---------------------------------------------------------------------------
// getMe
// ---------------------------------------------------------------------------

pub(crate) fn get_me(context: &RpcContext, _request: &RpcRequest) -> Result<Value, RpcError> {
    match context.permission {
        Permission::Admin => {
            let user = context.user.as_ref();
            Ok(json!({
                "2fa_enabled": user.is_some_and(|user| !user.two_factor.is_empty()),
                "logged_in": true,
                "sso_id": user.map(|user| user.sso_id.as_str()).unwrap_or(""),
                "sso_type": user.map(|user| user.sso_type.as_str()).unwrap_or(""),
                "username": user.map(|user| user.username.as_str()).unwrap_or(""),
                "uuid": user
                    .map(|user| user.uuid.as_str())
                    .unwrap_or(context.user_uuid.as_str()),
            }))
        }
        Permission::Client => Ok(json!({
            "2fa_enabled": false,
            "logged_in": true,
            "sso_id": "client",
            "sso_type": "client",
            "username": "client",
            // Go assigns `meta.ClientToken` and only overwrites it on a lookup
            // error, so the observable value is the token.
            "uuid": context.client_token,
        })),
        Permission::Guest => Ok(json!({
            "2fa_enabled": false,
            "logged_in": false,
            "sso_id": "",
            "sso_type": "",
            "username": "",
            "uuid": "",
        })),
    }
}

// ---------------------------------------------------------------------------
// getPublicInfo / getVersion
// ---------------------------------------------------------------------------

pub(crate) fn get_public_info(
    context: &RpcContext,
    _request: &RpcRequest,
) -> Result<Value, RpcError> {
    let db = &context.db;
    let theme = config_string(db, komari_config::keys::THEME_KEY, "default");
    let theme_settings = db
        .themes()
        .get_data(&theme)
        .unwrap_or_else(|_| Value::Object(Default::default()));

    Ok(json!({
        "sitename": config_string(db, komari_config::keys::SITENAME_KEY, "Komari"),
        "description": config_string(
            db,
            komari_config::keys::DESCRIPTION_KEY,
            "Komari Monitor, a simple server monitoring tool.",
        ),
        "custom_head": config_string(db, komari_config::keys::CUSTOM_HEAD_KEY, ""),
        "custom_body": config_string(db, komari_config::keys::CUSTOM_BODY_KEY, ""),
        "oauth_enable": config_bool(db, komari_config::keys::OAUTH_ENABLED_KEY, false),
        "oauth_provider": config_string(
            db,
            komari_config::keys::OAUTH_PROVIDER_KEY,
            "github",
        ),
        "disable_password_login": config_bool(
            db,
            komari_config::keys::DISABLE_PASSWORD_LOGIN_KEY,
            false,
        ),
        "allow_cors": config_bool(db, komari_config::keys::ALLOW_CORS_KEY, false),
        "record_enabled": config_bool(db, komari_config::keys::RECORD_ENABLED_KEY, true),
        "record_preserve_time": config_i64(
            db,
            komari_config::keys::RECORD_PRESERVE_TIME_KEY,
            720,
        ),
        "private_site": config_bool(db, komari_config::keys::PRIVATE_SITE_KEY, false),
        "theme": theme,
        "theme_settings": theme_settings,
    }))
}

fn config_string(db: &Db, key: &str, default: &str) -> String {
    db.config()
        .get_as::<String>(key)
        .ok()
        .flatten()
        .unwrap_or_else(|| default.to_owned())
}

fn config_bool(db: &Db, key: &str, default: bool) -> bool {
    db.config()
        .get_as::<bool>(key)
        .ok()
        .flatten()
        .unwrap_or(default)
}

fn config_i64(db: &Db, key: &str, default: i64) -> i64 {
    db.config()
        .get_as::<i64>(key)
        .ok()
        .flatten()
        .unwrap_or(default)
}

pub(crate) fn get_version(_context: &RpcContext, _request: &RpcRequest) -> Result<Value, RpcError> {
    Ok(json!({
        "version": VERSION,
        "hash": VERSION_HASH,
    }))
}

// ---------------------------------------------------------------------------
// getNodeRecentStatus
// ---------------------------------------------------------------------------

#[derive(Default, serde::Deserialize)]
struct RecentParams {
    #[serde(default)]
    uuid: String,
}

pub(crate) fn get_node_recent_status(
    context: &RpcContext,
    request: &RpcRequest,
) -> Result<Value, RpcError> {
    let params: RecentParams = request.bind_params();
    if params.uuid.is_empty() {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "UUID is required",
            Some(json!({"uuid": params.uuid})),
        ));
    }

    if context.permission != Permission::Admin {
        let hidden = hidden_clients(&context.db)?;
        if hidden.contains(&params.uuid) {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "UUID is required",
                Some(json!({"uuid": params.uuid})),
            ));
        }
    }

    let reports = context
        .report_cache
        .recent_reports()
        .remove(&params.uuid)
        .unwrap_or_default();
    let records = reports
        .iter()
        .map(|report| {
            json!({
                "client": params.uuid,
                "time": report_time(report),
                "cpu": report.cpu.usage,
                "gpu": 0,
                "ram": report.ram.used,
                "ram_total": report.ram.total,
                "swap": report.swap.used,
                "swap_total": report.swap.total,
                "load": report.load.load1,
                "temp": 0,
                "disk": report.disk.used,
                "disk_total": report.disk.total,
                "net_in": report.network.down,
                "net_out": report.network.up,
                "net_total_up": report.network.total_up,
                "net_total_down": report.network.total_down,
                "process": report.process,
                "connections": report.connections.tcp + report.connections.udp,
                "connections_udp": report.connections.udp,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "count": records.len(),
        "records": records,
    }))
}

fn hidden_clients(db: &Db) -> Result<HashSet<String>, RpcError> {
    let clients = db.clients().list().map_err(|error| {
        RpcError::new(
            INTERNAL_ERROR,
            "Failed to get client info",
            Some(json!(error.to_string())),
        )
    })?;
    Ok(clients
        .into_iter()
        .filter(|client| client.hidden)
        .map(|client| client.uuid)
        .collect())
}

// ---------------------------------------------------------------------------
// getRecords
// ---------------------------------------------------------------------------

#[derive(Default, serde::Deserialize)]
struct RecordsParams {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    uuid: String,
    #[serde(default)]
    hours: i64,
    #[serde(default)]
    start: String,
    #[serde(default)]
    end: String,
    #[serde(default)]
    load_type: String,
    #[serde(default, rename = "maxCount")]
    max_count: i64,
}

pub(crate) fn get_records(context: &RpcContext, request: &RpcRequest) -> Result<Value, RpcError> {
    let mut params: RecordsParams = request.bind_params();
    if params.kind.is_empty() {
        params.kind = "load".to_owned();
    }

    let (start, end) = time_window(&params)?;

    let hidden = if context.permission == Permission::Admin {
        HashSet::new()
    } else {
        hidden_clients(&context.db)?
    };
    if !params.uuid.is_empty() && hidden.contains(&params.uuid) {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "UUID not found",
            Some(Value::String(params.uuid)),
        ));
    }

    if params.kind != "load" {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "Invalid type, expected 'load'",
            Some(Value::String(params.kind)),
        ));
    }

    let clients = context.db.clients().list().map_err(|error| {
        RpcError::new(
            INTERNAL_ERROR,
            "Failed to get client info",
            Some(json!(error.to_string())),
        )
    })?;

    let mut grouped: HashMap<String, Vec<komari_db::models::Record>> = HashMap::new();
    for client in clients {
        if !params.uuid.is_empty() && client.uuid != params.uuid {
            continue;
        }
        if hidden.contains(&client.uuid) {
            continue;
        }
        let records = context
            .db
            .records()
            .combined_by_client_and_time(&client.uuid, start, end)
            .map_err(|error| {
                RpcError::new(
                    INTERNAL_ERROR,
                    "Failed to fetch records",
                    Some(json!(error.to_string())),
                )
            })?;
        if !records.is_empty() {
            grouped.insert(client.uuid, records);
        }
    }

    let max_count = if params.max_count == 0 {
        4000
    } else {
        params.max_count
    };

    if !params.load_type.is_empty() && params.load_type != "all" {
        let mut flat: HashMap<String, Vec<Value>> = HashMap::new();
        for (uuid, records) in &grouped {
            flat.insert(
                uuid.clone(),
                records
                    .iter()
                    .map(|record| filter_record(record, &params.load_type))
                    .collect(),
            );
        }
        downsample_groups(&mut flat, max_count);
        let count: usize = flat.values().map(Vec::len).sum();
        let records = flat
            .into_iter()
            .map(|(uuid, values)| (uuid, Value::Array(values)))
            .collect::<serde_json::Map<_, _>>();
        return Ok(json!({
            "count": count,
            "records": Value::Object(records),
            "load_type": params.load_type,
            "from": start,
            "to": end,
        }));
    }

    for records in grouped.values_mut() {
        records.sort_by_key(|record| record.time);
    }
    downsample_records(&mut grouped, max_count);
    let count: usize = grouped.values().map(Vec::len).sum();
    let records = grouped
        .into_iter()
        .map(|(uuid, values)| {
            (
                uuid,
                serde_json::to_value(values).unwrap_or(Value::Array(Vec::new())),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    Ok(json!({
        "count": count,
        "records": Value::Object(records),
        "from": start,
        "to": end,
    }))
}

fn time_window(params: &RecordsParams) -> Result<(LocalTime, LocalTime), RpcError> {
    if !params.start.is_empty() || !params.end.is_empty() {
        let end = if params.end.is_empty() {
            LocalTime::now()
        } else {
            parse_time(&params.end).ok_or_else(|| {
                RpcError::new(
                    INVALID_PARAMS,
                    "Invalid end time",
                    Some(Value::String(params.end.clone())),
                )
            })?
        };
        let start = if params.start.is_empty() {
            end.add_seconds(-3600)
        } else {
            parse_time(&params.start).ok_or_else(|| {
                RpcError::new(
                    INVALID_PARAMS,
                    "Invalid start time",
                    Some(Value::String(params.start.clone())),
                )
            })?
        };
        return Ok((start, end));
    }

    let hours = if params.hours <= 0 { 1 } else { params.hours };
    let end = LocalTime::now();
    let start = end.add_seconds(-hours.saturating_mul(3600));
    Ok((start, end))
}

fn parse_time(value: &str) -> Option<LocalTime> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(LocalTime::from_datetime)
}

fn filter_record(record: &komari_db::models::Record, load_type: &str) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("client".to_owned(), Value::String(record.client.clone()));
    object.insert(
        "time".to_owned(),
        serde_json::to_value(record.time).unwrap_or(Value::Null),
    );
    let mut insert = |key: &str, value: Value| {
        object.insert(key.to_owned(), value);
    };
    match load_type {
        "cpu" => insert("cpu", json!(record.cpu)),
        "gpu" => insert("gpu", json!(record.gpu)),
        "ram" => {
            insert("ram", json!(record.ram));
            insert("ram_total", json!(record.ram_total));
        }
        "swap" => {
            insert("swap", json!(record.swap));
            insert("swap_total", json!(record.swap_total));
        }
        "load" => insert("load", json!(record.load)),
        "temp" => insert("temp", json!(record.temp)),
        "disk" => {
            insert("disk", json!(record.disk));
            insert("disk_total", json!(record.disk_total));
        }
        "network" => {
            insert("net_in", json!(record.net_in));
            insert("net_out", json!(record.net_out));
            insert("net_total_up", json!(record.net_total_up));
            insert("net_total_down", json!(record.net_total_down));
        }
        "process" => insert("process", json!(record.process)),
        "connections" => {
            insert("connections", json!(record.connections));
            insert("connections_udp", json!(record.connections_udp));
        }
        _ => insert("cpu", json!(record.cpu)),
    }
    Value::Object(object)
}

fn downsample_records(
    grouped: &mut HashMap<String, Vec<komari_db::models::Record>>,
    max_count: i64,
) {
    if max_count < 0 {
        return;
    }
    let total: usize = grouped.values().map(Vec::len).sum();
    if total <= max_count as usize {
        return;
    }
    let targets = allocate_targets(grouped, max_count as usize);
    for (uuid, records) in grouped.iter_mut() {
        let target = targets.get(uuid).copied().unwrap_or(0);
        *records = downsample_vec(records, target);
    }
}

fn downsample_groups(grouped: &mut HashMap<String, Vec<Value>>, max_count: i64) {
    if max_count < 0 {
        return;
    }
    let total: usize = grouped.values().map(Vec::len).sum();
    if total <= max_count as usize {
        return;
    }
    let lengths = grouped
        .iter()
        .map(|(uuid, values)| (uuid.clone(), values.len()))
        .collect::<HashMap<_, _>>();
    let targets = allocate_targets_from(&lengths, max_count as usize);
    for (uuid, values) in grouped.iter_mut() {
        let target = targets.get(uuid).copied().unwrap_or(0);
        *values = downsample_vec(values, target);
    }
}

fn allocate_targets<T>(
    grouped: &HashMap<String, Vec<T>>,
    max_total: usize,
) -> HashMap<String, usize> {
    let lengths = grouped
        .iter()
        .map(|(uuid, values)| (uuid.clone(), values.len()))
        .collect::<HashMap<_, _>>();
    allocate_targets_from(&lengths, max_total)
}

fn allocate_targets_from(
    lengths: &HashMap<String, usize>,
    max_total: usize,
) -> HashMap<String, usize> {
    let total: usize = lengths.values().sum();
    let mut result = HashMap::with_capacity(lengths.len());
    if total <= max_total {
        for (uuid, length) in lengths {
            result.insert(uuid.clone(), *length);
        }
        return result;
    }

    let mut remainders: Vec<(String, f64)> = Vec::with_capacity(lengths.len());
    let mut assigned = 0usize;
    for (uuid, length) in lengths {
        if *length == 0 {
            result.insert(uuid.clone(), 0);
            continue;
        }
        let raw = *length as f64 * max_total as f64 / total as f64;
        let target = raw.floor() as usize;
        let target = target.min(*length);
        result.insert(uuid.clone(), target);
        assigned += target;
        remainders.push((uuid.clone(), raw - raw.floor()));
    }

    if assigned < max_total {
        remainders.sort_by(|left, right| right.1.total_cmp(&left.1));
        let mut need = max_total - assigned;
        for (uuid, _) in &remainders {
            if need == 0 {
                break;
            }
            if let (Some(current), Some(length)) = (result.get_mut(uuid), lengths.get(uuid)) {
                if *current < *length {
                    *current += 1;
                    need -= 1;
                }
            }
        }
    } else if assigned > max_total {
        remainders.sort_by(|left, right| left.1.total_cmp(&right.1));
        let mut over = assigned - max_total;
        for (uuid, _) in &remainders {
            if over == 0 {
                break;
            }
            if let Some(current) = result.get_mut(uuid) {
                if *current > 0 {
                    *current -= 1;
                    over -= 1;
                }
            }
        }
    }
    result
}

fn downsample_vec<T: Clone>(input: &[T], target: usize) -> Vec<T> {
    let length = input.len();
    if target == 0 || length == 0 {
        return Vec::new();
    }
    if target >= length {
        return input.to_vec();
    }
    if target == 1 {
        return vec![input[length - 1].clone()];
    }
    let mut output = Vec::with_capacity(target);
    for index in 0..target {
        let position =
            ((index as f64) * ((length - 1) as f64) / ((target - 1) as f64)).round() as usize;
        output.push(input[position.min(length - 1)].clone());
    }
    output
}
