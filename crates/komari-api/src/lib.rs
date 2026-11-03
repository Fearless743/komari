//! Agent-facing HTTP and WebSocket API.
//!
//! This crate implements the routes the Komari agent uses:
//!
//! * `POST /api/clients/register` — auto-discovery registration.
//! * `GET  /api/clients/report` — persistent WebSocket metric stream.
//! * `POST /api/clients/report` — one-shot HTTP metric report.
//! * `POST /api/clients/uploadBasicInfo` — static hardware/OS information.
//! * `GET  /api/clients` — browser WebSocket used by the dashboard.
//!
//! Public routes, the admin API and JSON-RPC intentionally live in other
//! crates/tasks; [`client_router`] can be merged into a larger router.

mod agent;
mod clock;
mod presence;
mod report;
mod response;
mod state;
pub mod ws;

pub use clock::{Clock, ManualClock, SystemClock};
pub use presence::{PresenceExpiry, PresenceTracker, PresenceUpdate};
pub use report::{
    ConnectionsReport, CpuReport, DiskReport, GpuDetailReport, GpuDeviceInfo, LoadReport,
    NetworkReport, RamReport, Report, save_report, verify_report,
};
pub use state::{ApiConfig, ApiState};
pub use ws::{ClientEvent, ClientEventKind, ConnectionHandle, PresenceSource, WsHub, check_origin};

use axum::Router;
use axum::routing::{get, post};

/// Routes owned by the agent API and the dashboard's client WebSocket.
///
/// The returned router still needs an [`ApiState`]. Compose it with other
/// routers that share the same state, then call `with_state` once:
///
/// ```no_run
/// use axum::Router;
/// use komari_api::{ApiState, client_router};
///
/// # fn build(state: ApiState) -> Router {
/// Router::new().merge(client_router()).with_state(state)
/// # }
/// ```
pub fn client_router() -> Router<ApiState> {
    Router::new()
        .route("/api/clients", get(ws::clients_ws))
        .route(
            "/api/clients/register",
            post(agent::register::register_client),
        )
        .route(
            "/api/clients/report",
            get(ws::agent_ws).post(agent::report::upload_report),
        )
        .route(
            "/api/clients/uploadBasicInfo",
            post(agent::basic_info::upload_basic_info),
        )
}

/// Convenience helper for binaries that do not compose several routers.
pub fn client_router_with_state(state: ApiState) -> Router {
    client_router().with_state(state)
}
