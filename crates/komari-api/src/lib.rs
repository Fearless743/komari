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
mod cache;
mod clock;
mod presence;
mod report;
mod response;
pub mod server;
mod state;
pub mod ws;

pub use cache::{FlushSummary, ReportCache};
pub use clock::{Clock, ManualClock, SystemClock};
pub use presence::{PresenceExpiry, PresenceTracker, PresenceUpdate};
pub use report::{
    ConnectionsReport, CpuReport, DiskReport, GpuDetailReport, GpuDeviceInfo, LoadReport,
    NetworkReport, RamReport, Report, aggregate_reports, save_report, verify_report,
};
pub use server::{
    BackgroundConfig, BackgroundTasks, OAuthRuntime, Server, ServerBuilder, ServerConfig,
};
pub use state::{ApiConfig, ApiState};
pub use ws::{ClientEvent, ClientEventKind, ConnectionHandle, PresenceSource, WsHub, check_origin};

use axum::Router;
use axum::routing::{get, post};
use komari_auth::{AuthLayer, RequireRole};

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
        .merge(agent_routes())
}

/// The `/api/clients` group that requires `Admin` or `Client`, matching Go.
fn agent_routes() -> Router<ApiState> {
    Router::new()
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
///
/// In addition to the handlers' own B03-based checks, this installs B03's
/// [`AuthLayer`] and [`RequireRole`] around the agent group so body tokens are
/// resolved once and the cached [`komari_auth::AuthContext`] is reused.
pub fn client_router_with_state(state: ApiState) -> Router {
    let auth = state.auth.clone();
    let agent = agent_routes()
        .layer(RequireRole::admin_or_client(auth.clone()))
        .layer(AuthLayer::new(auth));
    Router::new()
        .route("/api/clients", get(ws::clients_ws))
        .route(
            "/api/clients/register",
            post(agent::register::register_client),
        )
        .merge(agent)
        .with_state(state)
}
