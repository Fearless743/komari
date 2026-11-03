//! Embedded default theme and static asset serving for Komari.
//!
//! This crate owns the pieces the HTTP server needs to serve the web UI:
//!
//! * the default theme is embedded with [`rust-embed`], so a release binary is
//!   self-contained;
//! * `data/theme/<short>` themes take precedence over the embedded files, and
//!   missing files fall back to the default theme, matching the Go backend;
//! * `GET /favicon.ico` prefers `data/favicon.ico`, then the active theme;
//! * unknown non-`/api` routes return `dist/index.html` (SPA fallback), with
//!   site name/description/custom head/custom body replacements;
//! * static responses carry `Content-Type`, `Cache-Control`, `ETag` and support
//!   conditional requests and optional pre-compressed `.gz`/`.br` siblings.
//!
//! # Integration
//!
//! ```no_run
//! use std::sync::Arc;
//! use komari_web::{WebAssets, WebConfig};
//!
//! let assets = Arc::new(WebAssets::new(WebConfig::default()));
//! let app = komari_web::routes(assets.clone())
//!     .fallback(move |request| {
//!         let assets = assets.clone();
//!         async move { komari_web::serve_fallback(&assets, request).await }
//!     });
//! ```
//!
//! [`router`] builds the same routes plus the fallback for standalone use.

#![forbid(unsafe_code)]

mod asset;
mod html;
mod layout;
mod server;
mod theme;

pub use asset::{Asset, CachePolicy, accepts_encoding, content_type_for, etag_for, etag_matches};
pub use html::{DEFAULT_DESCRIPTION, DEFAULT_TITLE, apply_site_replacements};
pub use layout::{ThemeLayout, ThemeLayoutError, validate_theme_layout};
pub use server::{
    SiteMeta, WebAssets, WebConfig, is_safe_theme_id, router, router_for, routes, routes_for,
    sanitize_relative, serve_fallback, serve_favicon, serve_theme_asset,
};
pub use theme::{ThemeError, ThemeInfo};

use rust_embed::Embed;

/// Theme short name of the built-in theme.
pub const DEFAULT_THEME: &str = "default";
/// Directory below the data directory that holds installed themes.
pub const THEMES_DIR: &str = "theme";
/// User-provided favicon, relative to the data directory.
pub const FAVICON_FILE: &str = "favicon.ico";
/// Static files inside a theme.
pub const DIST_DIR: &str = "dist";
/// SPA entry point inside [`DIST_DIR`].
pub const INDEX_FILE: &str = "index.html";
/// Theme metadata file.
pub const THEME_CONFIG_FILE: &str = "komari-theme.json";
/// Accepted preview image names (the second is the historical typo).
pub const PREVIEW_FILES: [&str; 2] = ["preview.png", "perview.png"];

/// The default theme baked into the binary.
///
/// `build.rs` resolves `KOMARI_DEFAULT_THEME_DIR` (falling back to the
/// committed placeholder) and exports `KOMARI_EMBEDDED_THEME_DIR` for the
/// derive macro.
#[derive(Embed)]
#[folder = "$KOMARI_EMBEDDED_THEME_DIR"]
struct DefaultThemeAssets;
