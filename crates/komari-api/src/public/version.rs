//! `GET /api/version` — build version and commit hash.
//!
//! Port of Go `api/public.GetVersion`. The constants mirror
//! `komari-cli/src/version.rs`: `KOMARI_VERSION` / `KOMARI_VERSION_HASH`
//! override the Cargo package version and the `unknown` development marker.

use axum::response::Response;
use serde_json::json;

use super::respond_success;

/// Build version. `KOMARI_VERSION` wins over the Cargo package version.
pub const VERSION: &str = match option_env!("KOMARI_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Git commit hash. `unknown` identifies a development build.
pub const VERSION_HASH: &str = match option_env!("KOMARI_VERSION_HASH") {
    Some(hash) => hash,
    None => "unknown",
};

pub(crate) async fn get_version() -> Response {
    respond_success(json!({
        "version": VERSION,
        "hash": VERSION_HASH,
    }))
}
