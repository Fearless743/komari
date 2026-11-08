//! Permission detection and execution context for `/api/rpc2`.
//!
//! This ports `detectPermissionGroup` / `buildContextMeta` from
//! `api/jsonRpc/JsonRpc.go`: the query parameter `Authorization` carries an
//! agent token, the `session_token` cookie identifies an administrator, and
//! `Authorization: Bearer <api_key>` also grants admin.

use std::sync::Arc;

use axum::http::header::{AUTHORIZATION, COOKIE, USER_AGENT};
use axum::http::{HeaderMap, Uri};
use komari_db::Db;
use komari_db::models::User;

use crate::cache::ReportCache;
use crate::ws::WsHub;

/// Permission namespace used by the RPC registry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    #[default]
    Guest,
    Client,
    Admin,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Guest => "guest",
            Self::Client => "client",
            Self::Admin => "admin",
        }
    }

    /// Whether a namespace may be called by this permission group.
    pub fn allows_namespace(self, namespace: &str) -> bool {
        match namespace {
            "guest" | "" | "rpc" | "common" => true,
            "client" => self >= Self::Client,
            "admin" => self == Self::Admin,
            _ => false,
        }
    }
}

/// Identity and request metadata available to RPC handlers.
#[derive(Clone)]
pub struct RpcContext {
    pub db: Arc<Db>,
    pub hub: Arc<WsHub>,
    pub report_cache: Arc<ReportCache>,
    pub permission: Permission,
    pub user: Option<User>,
    pub user_uuid: String,
    pub client_token: String,
    pub client_uuid: String,
    pub remote_ip: String,
    pub user_agent: String,
}

impl RpcContext {
    pub fn is_admin(&self) -> bool {
        self.permission == Permission::Admin
    }

    pub fn is_guest(&self) -> bool {
        self.permission == Permission::Guest
    }
}

/// Resolve the permission group and metadata for one request.
pub async fn detect(
    db: Arc<Db>,
    hub: Arc<WsHub>,
    report_cache: Arc<ReportCache>,
    headers: &HeaderMap,
    uri: &Uri,
) -> RpcContext {
    let mut permission = Permission::Guest;
    let mut user = None;
    let mut user_uuid = String::new();
    let mut client_token = String::new();
    let mut client_uuid = String::new();

    // 1. API key (admin).
    let authorization = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim);
    let bearer = authorization.and_then(|value| value.strip_prefix("Bearer "));
    if let (Some(configured), Some(token)) = (configured_api_key(&db), bearer) {
        if token == configured {
            permission = Permission::Admin;
        }
    }

    // 2. Session cookie (admin).
    if let Some(session) = cookie(headers, komari_auth::SESSION_COOKIE_NAME) {
        if !session.is_empty() {
            if let Ok(Some(uuid)) = db.sessions().validate(&session) {
                if let Ok(Some(found)) = db.accounts().get_by_uuid(&uuid) {
                    permission = Permission::Admin;
                    user_uuid = found.uuid.clone();
                    user = Some(found);
                }
            }
        }
    }

    // 3. Agent token: the query parameter determines the permission group,
    // matching Go `detectPermissionGroup`. The Bearer header is only used for
    // context metadata (Go `buildContextMeta`), not for authorization.
    let query_token = query_param(uri.query(), "Authorization").filter(|value| !value.is_empty());
    if let Some(token) = query_token {
        if let Ok(Some(client)) = db.clients().get_by_token(&token) {
            client_token = token;
            client_uuid = client.uuid;
            if permission != Permission::Admin {
                permission = Permission::Client;
            }
        }
    } else if let Some(token) = bearer {
        if let Ok(Some(client)) = db.clients().get_by_token(token) {
            client_token = token.to_owned();
            client_uuid = client.uuid;
        }
    }

    let remote_ip = client_ip(headers);
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();

    RpcContext {
        db,
        hub,
        report_cache,
        permission,
        user,
        user_uuid,
        client_token,
        client_uuid,
        remote_ip,
        user_agent,
    }
}

fn configured_api_key(db: &Db) -> Option<String> {
    let key = db.config().get_as::<String>("api_key").ok().flatten()?;
    // Must match the main middleware (`MIN_API_KEY_LENGTH = 12`): short keys
    // are treated as unconfigured everywhere, including `/api/rpc2`.
    (key.len() >= 12).then_some(key)
}

pub(crate) fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name).then(|| value.to_owned())
        })
        .next()
}

fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    let query = query?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(key) == name).then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        if bytes[index] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[index]);
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Go `c.ClientIP` approximation for axum, which has no peer address here.
pub(crate) fn client_ip(headers: &HeaderMap) -> String {
    for name in ["CF-Connecting-IP", "X-Forwarded-For", "X-Real-IP"] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            let first = value.split(',').next().unwrap_or("").trim();
            if !first.is_empty() {
                return first.to_owned();
            }
        }
    }
    "127.0.0.1".to_owned()
}
