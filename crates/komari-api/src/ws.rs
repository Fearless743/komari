//! WebSocket infrastructure shared by the agent stream and the dashboard.
//!
//! [`WsHub`] mirrors the bookkeeping the Go backend keeps in `ws/*.go`:
//! per-client connections, the latest report cache, and presence for agents
//! that report over plain HTTP POST. Presence is kept in milliseconds via the
//! injectable [`Clock`](crate::Clock) so tests are deterministic.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use komari_auth::Role;
use serde_json::json;
use tokio::sync::{broadcast, oneshot};

use crate::clock::Clock;
use crate::report::Report;
use crate::response::error_json;
use crate::state::ApiState;

/// Where an online/offline event originated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresenceSource {
    WebSocket,
    HttpPost,
}

/// Online/offline event published by the hub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientEvent {
    pub kind: ClientEventKind,
    pub uuid: String,
    pub connection_id: i64,
    pub source: PresenceSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientEventKind {
    Online,
    Offline,
}

/// A live WebSocket connection, used to close a client's previous stream when
/// it reconnects.
pub struct ConnectionHandle {
    pub connection_id: i64,
    closed: std::sync::atomic::AtomicBool,
    close_tx: Mutex<Option<oneshot::Sender<()>>>,
}

impl std::fmt::Debug for ConnectionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionHandle")
            .field("connection_id", &self.connection_id)
            .field("closed", &self.closed.load(Ordering::SeqCst))
            .finish()
    }
}

impl ConnectionHandle {
    fn new(connection_id: i64) -> (Self, oneshot::Receiver<()>) {
        let (close_tx, close_rx) = oneshot::channel();
        (
            Self {
                connection_id,
                closed: std::sync::atomic::AtomicBool::new(false),
                close_tx: Mutex::new(Some(close_tx)),
            },
            close_rx,
        )
    }

    /// Ask the owning task to stop. Idempotent.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let sender = self
            .close_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(sender) = sender {
            let _ = sender.send(());
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

struct Presence {
    connection_id: i64,
    expires_at_millis: i64,
}

#[derive(Default)]
struct HubInner {
    connected: HashMap<String, Arc<ConnectionHandle>>,
    latest_reports: HashMap<String, Report>,
    presence: HashMap<String, Presence>,
}

/// Per-client connection bookkeeping plus online/offline events.
pub struct WsHub {
    clock: Arc<dyn Clock>,
    inner: RwLock<HubInner>,
    events: broadcast::Sender<ClientEvent>,
    next_id: AtomicI64,
}

impl WsHub {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            clock,
            inner: RwLock::new(HubInner::default()),
            events,
            next_id: AtomicI64::new(0),
        }
    }

    /// Subscribe to online/offline events. Slow consumers lose old events.
    pub fn subscribe(&self) -> broadcast::Receiver<ClientEvent> {
        self.events.subscribe()
    }

    /// Allocate a connection id, unique even when the clock is frozen.
    pub fn next_connection_id(&self) -> i64 {
        let sequence = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.clock
            .now_millis()
            .saturating_mul(1_000_000)
            .saturating_add(sequence)
    }

    /// Register a WebSocket connection. A previous connection for the same
    /// client is returned (and already closed), matching the Go reconnect
    /// behaviour.
    pub fn register_ws(
        &self,
        uuid: impl Into<String>,
        handle: Arc<ConnectionHandle>,
    ) -> Option<Arc<ConnectionHandle>> {
        let uuid = uuid.into();
        let previous = {
            let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
            inner.connected.insert(uuid.clone(), Arc::clone(&handle))
        };
        if let Some(previous) = previous.as_ref() {
            previous.close();
        }
        self.emit(ClientEvent {
            kind: ClientEventKind::Online,
            uuid,
            connection_id: handle.connection_id,
            source: PresenceSource::WebSocket,
        });
        previous
    }

    /// Remove a connection only if it is still the current one. Returns `true`
    /// when the hub had to remove it (and therefore published an offline
    /// event).
    pub fn unregister_ws(&self, uuid: &str, connection_id: i64) -> bool {
        let removed = {
            let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
            let is_current = inner
                .connected
                .get(uuid)
                .is_some_and(|handle| handle.connection_id == connection_id);
            if is_current {
                inner.connected.remove(uuid);
            }
            is_current
        };
        if removed {
            self.emit(ClientEvent {
                kind: ClientEventKind::Offline,
                uuid: uuid.to_owned(),
                connection_id,
                source: PresenceSource::WebSocket,
            });
        }
        removed
    }

    /// Refresh (or create) POST-report presence. Callers are responsible for
    /// publishing the first online event (see [`crate::PresenceTracker`]).
    pub fn keep_alive_presence(&self, uuid: &str, connection_id: i64, ttl: Duration) {
        let expires_at_millis = self
            .clock
            .now_millis()
            .saturating_add(ttl.as_millis() as i64);
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        inner.presence.insert(
            uuid.to_owned(),
            Presence {
                connection_id,
                expires_at_millis,
            },
        );
    }

    /// Clear POST presence when the connection id still matches.
    pub fn clear_presence(&self, uuid: &str, connection_id: i64) -> bool {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        let matches = inner
            .presence
            .get(uuid)
            .is_some_and(|presence| presence.connection_id == connection_id);
        if matches {
            inner.presence.remove(uuid);
        }
        matches
    }

    pub fn emit_online(&self, uuid: impl Into<String>, connection_id: i64, source: PresenceSource) {
        self.emit(ClientEvent {
            kind: ClientEventKind::Online,
            uuid: uuid.into(),
            connection_id,
            source,
        });
    }

    pub fn emit_offline(
        &self,
        uuid: impl Into<String>,
        connection_id: i64,
        source: PresenceSource,
    ) {
        self.emit(ClientEvent {
            kind: ClientEventKind::Offline,
            uuid: uuid.into(),
            connection_id,
            source,
        });
    }

    fn emit(&self, event: ClientEvent) {
        let _ = self.events.send(event);
    }

    /// De-duplicated, sorted list of online UUIDs from both WebSocket and HTTP
    /// POST agents.
    pub fn online_uuids(&self) -> Vec<String> {
        let now = self.clock.now_millis();
        let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let mut set: BTreeSet<String> = inner.connected.keys().cloned().collect();
        set.extend(
            inner
                .presence
                .iter()
                .filter(|(_, presence)| presence.expires_at_millis > now)
                .map(|(uuid, _)| uuid.clone()),
        );
        set.into_iter().collect()
    }

    pub fn is_online(&self, uuid: &str) -> bool {
        let now = self.clock.now_millis();
        let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
        inner.connected.contains_key(uuid)
            || inner
                .presence
                .get(uuid)
                .is_some_and(|presence| presence.expires_at_millis > now)
    }

    pub fn connected_count(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .connected
            .len()
    }

    pub fn connection_id(&self, uuid: &str) -> Option<i64> {
        self.inner
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .connected
            .get(uuid)
            .map(|handle| handle.connection_id)
    }

    /// Store the latest report for a client.
    pub fn set_latest_report(&self, uuid: &str, report: Report) {
        self.inner
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .latest_reports
            .insert(uuid.to_owned(), report);
    }

    pub fn latest_report(&self, uuid: &str) -> Option<Report> {
        self.inner
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .latest_reports
            .get(uuid)
            .cloned()
    }

    pub fn latest_reports(&self) -> HashMap<String, Report> {
        self.inner
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .latest_reports
            .clone()
    }

    pub fn remove_latest_report(&self, uuid: &str) {
        self.inner
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .latest_reports
            .remove(uuid);
    }
}

/// Check a browser WebSocket `Origin` against the request `Host`, matching
/// `ws.CheckOrigin`.
pub fn check_origin(headers: &HeaderMap, disable: bool) -> bool {
    if disable {
        return true;
    }
    let Some(origin) = headers
        .get(http::header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    if origin.is_empty() {
        return false;
    }
    let Ok(uri) = origin.parse::<http::Uri>() else {
        return false;
    };
    if uri.scheme().is_none() {
        return false;
    }
    let Some(origin_host) = uri.authority().map(|authority| authority.as_str()) else {
        return false;
    };
    let Some(host) = headers
        .get(http::header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    origin_host.eq_ignore_ascii_case(host)
}

/// `GET /api/clients/report` — agent WebSocket.
pub async fn agent_ws(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let token = komari_auth::extract_agent_token(&headers, query.get("token").map(String::as_str));
    let identity = match komari_auth::require_role(&state.db, token.as_deref(), &[Role::Client]) {
        Ok(identity) => identity,
        Err(komari_auth::RequireRoleError::Backend(error)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_json(format!("Failed to authenticate client: {error}")),
            )
                .into_response();
        }
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({"status": "error", "message": "Unauthorized."})),
            )
                .into_response();
        }
    };

    ws.on_upgrade(move |socket| handle_agent_socket(state, identity.uuid, socket))
}

async fn handle_agent_socket(state: ApiState, uuid: String, mut socket: WebSocket) {
    let connection_id = state.hub.next_connection_id();
    let (handle, mut close_rx) = ConnectionHandle::new(connection_id);
    state.hub.register_ws(uuid.clone(), Arc::new(handle));

    let timeout = state.config().socket_read_timeout;
    loop {
        tokio::select! {
            _ = &mut close_rx => break,
            received = tokio::time::timeout(timeout, socket.recv()) => {
                match received {
                    Err(_) | Ok(None) | Ok(Some(Err(_))) => break,
                    Ok(Some(Ok(message))) => {
                        let keep_going = match message {
                            Message::Text(text) => process_agent_message(&state, &uuid, text.as_bytes()),
                            Message::Binary(bytes) => process_agent_message(&state, &uuid, &bytes),
                            Message::Close(_) => false,
                            Message::Ping(_) | Message::Pong(_) => true,
                        };
                        if !keep_going {
                            break;
                        }
                    }
                }
            }
        }
    }

    state.hub.unregister_ws(&uuid, connection_id);
}

/// Parse one v1 agent frame. Returns `false` when the socket should close.
fn process_agent_message(state: &ApiState, uuid: &str, payload: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
        tracing::warn!(client = %uuid, "invalid agent message");
        return true;
    };

    #[derive(serde::Deserialize)]
    struct Envelope {
        #[serde(default, rename = "type")]
        kind: String,
    }
    let envelope = match serde_json::from_value::<Envelope>(value.clone()) {
        Ok(envelope) => envelope,
        Err(error) => {
            tracing::warn!(client = %uuid, %error, "invalid agent envelope");
            return true;
        }
    };

    match envelope.kind.as_str() {
        "" | "report" => {
            let Ok(mut report) = serde_json::from_value::<Report>(value) else {
                tracing::warn!(client = %uuid, "invalid report frame");
                return true;
            };
            report.uuid = uuid.to_owned();
            report.mark_updated();
            if let Err(error) = crate::report::save_report(&state.db, uuid, &report) {
                tracing::warn!(client = %uuid, %error, "failed to save report");
                return true;
            }
            state
                .hub
                .set_latest_report(uuid, crate::report::cache_report(report));
            true
        }
        other => {
            tracing::warn!(client = %uuid, kind = other, "unknown agent message type");
            true
        }
    }
}

/// `GET /api/clients` — browser WebSocket used by the dashboard.
///
/// Login/session handling and hidden-client filtering are completed in the web
/// task; this implementation exposes the same `get` / `get <uuid>` protocol so
/// the route is usable today.
pub async fn clients_ws(
    State(state): State<ApiState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !state.allow_cors() && !check_origin(&headers, state.ws_disable_origin()) {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(json!({"status": "error", "error": "Origin not allowed"})),
        )
            .into_response();
    }
    ws.on_upgrade(move |socket| handle_browser_socket(state, socket))
}

async fn handle_browser_socket(state: ApiState, mut socket: WebSocket) {
    while let Some(message) = socket.recv().await {
        let message = match message {
            Ok(message) => message,
            Err(_) => break,
        };
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(bytes) => match String::from_utf8(bytes.to_vec()) {
                Ok(text) => text.into(),
                Err(_) => {
                    if send_browser_error(&mut socket, "Invalid message")
                        .await
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
            },
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => continue,
        };

        let text = text.as_str();
        let filter = if text == "get" {
            None
        } else if let Some(rest) = text.strip_prefix("get ") {
            let rest = rest.trim();
            (!rest.is_empty()).then(|| rest.to_owned())
        } else {
            if send_browser_error(&mut socket, "Invalid message")
                .await
                .is_err()
            {
                break;
            }
            continue;
        };

        let online: Vec<String> = state
            .hub
            .online_uuids()
            .into_iter()
            .filter(|uuid| filter.as_deref().is_none_or(|wanted| wanted == uuid))
            .collect();
        let mut data = serde_json::Map::new();
        for (uuid, mut report) in state.hub.latest_reports() {
            if filter.as_deref().is_some_and(|wanted| wanted != uuid) {
                continue;
            }
            report.uuid.clear();
            if report.cpu.usage == 0.0 {
                report.cpu.usage = 0.01;
            }
            match serde_json::to_value(report) {
                Ok(value) => {
                    data.insert(uuid, value);
                }
                Err(error) => tracing::warn!(%error, "failed to serialize report"),
            }
        }

        let payload = json!({
            "status": "success",
            "data": {
                "online": online,
                "data": data,
            },
        });
        if socket
            .send(Message::Text(payload.to_string().into()))
            .await
            .is_err()
        {
            break;
        }
    }
}

async fn send_browser_error(socket: &mut WebSocket, message: &str) -> Result<(), axum::Error> {
    socket
        .send(Message::Text(
            json!({"status": "error", "error": message})
                .to_string()
                .into(),
        ))
        .await
}
