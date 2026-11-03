//! Static-asset wiring: construct [`WebAssets`] from startup configuration and
//! refresh theme/site/temporary-share values from the `configs` table.
//!
//! The Go backend reads these keys on every request. The Rust server keeps a
//! snapshot in [`WebAssets`] and refreshes it at startup, on a timer and after
//! an admin settings update (B06 can call [`refresh_web_assets`] directly).

use std::path::Path;
use std::sync::Arc;

use komari_config::keys;
use komari_db::{Db, DbError};
use komari_web::{SiteMeta, WebAssets, WebConfig};

use crate::server::ServerConfig;

/// Build the asset service for a server configuration.
pub fn build_web_assets(
    config: &ServerConfig,
    db: &Db,
) -> Result<Arc<WebAssets>, DbError> {
    let assets = Arc::new(WebAssets::new(WebConfig {
        data_dir: config.data_dir.clone(),
        theme: config.theme.clone(),
        ..WebConfig::default()
    }));
    refresh_web_assets(&assets, db)?;
    Ok(assets)
}

/// Re-read theme, site metadata and the temporary share token.
///
/// Missing keys keep the values already present in `assets`, matching Go's
/// `config.GetMany` defaults. A configured theme that is not installed is left
/// in place; [`WebAssets::effective_theme`] falls back to the embedded default
/// while serving.
pub fn refresh_web_assets(assets: &WebAssets, db: &Db) -> Result<(), DbError> {
    if let Some(theme) = db
        .config()
        .get_as::<String>(keys::THEME_KEY)?
        .filter(|theme| !theme.trim().is_empty())
    {
        assets.set_theme(theme);
    }

    let current = assets.site_meta();
    assets.set_site_meta(SiteMeta {
        sitename: read_string(db, keys::SITENAME_KEY)?.unwrap_or(current.sitename),
        description: read_string(db, keys::DESCRIPTION_KEY)?.unwrap_or(current.description),
        custom_head: read_string(db, keys::CUSTOM_HEAD_KEY)?.unwrap_or(current.custom_head),
        custom_body: read_string(db, keys::CUSTOM_BODY_KEY)?.unwrap_or(current.custom_body),
    });

    let token = read_string(db, TEMP_SHARE_TOKEN_KEY)?;
    let expires_at = db
        .config()
        .get_as::<i64>(TEMP_SHARE_EXPIRE_KEY)?
        .unwrap_or_default();
    match token.filter(|token| !token.is_empty()) {
        Some(token) => assets.set_temp_share_token(token, expires_at),
        None => assets.clear_temp_share_token(),
    }

    Ok(())
}

/// Persisted key holding the temporary share token (Go spelling).
pub const TEMP_SHARE_TOKEN_KEY: &str = "tempory_share_token";
/// Persisted key holding the temporary share expiry as Unix seconds.
pub const TEMP_SHARE_EXPIRE_KEY: &str = "tempory_share_token_expire_at";

/// Derive the data directory from a database path (`./data/komari.db` →
/// `./data`). Paths without a parent use `.` so pending-backup restore and
/// theme lookup still agree with the database location.
pub fn data_dir_for_database(path: &Path) -> std::path::PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => std::path::PathBuf::from("."),
    }
}

fn read_string(db: &Db, key: &str) -> Result<Option<String>, DbError> {
    db.config()
        .get_as::<String>(key)
        .map(|value| value.filter(|value| !value.is_empty()))
}
