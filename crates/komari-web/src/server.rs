//! Theme selection and Axum handlers for static assets, SPA fallback and
//! favicons.
//!
//! The behaviour mirrors the Go `public.Static` implementation:
//!
//! * a theme file is read from `data/theme/<short>/...` first and falls back to
//!   the embedded default theme;
//! * unknown non-API routes return `dist/index.html` with site replacements;
//! * `/admin` always uses the default theme and is not rewritten;
//! * `/api/*` never falls back to the SPA;
//! * `data/favicon.ico` wins over the theme favicon.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::Body,
    extract::{Path as AxumPath, Request},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::Response,
    routing::any,
};
use bytes::Bytes;
use percent_encoding::percent_decode_str;
use thiserror::Error;
use tracing::warn;

use crate::{
    DEFAULT_THEME, DIST_DIR, DefaultThemeAssets, FAVICON_FILE, INDEX_FILE, THEME_CONFIG_FILE,
    THEMES_DIR,
    asset::{Asset, CachePolicy, accepted_encodings, system_time_from_unix},
    html::apply_site_replacements,
    theme::ThemeInfo,
};

/// Runtime configuration for [`WebAssets`].
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// Root data directory, matching the Go `./data` layout.
    pub data_dir: PathBuf,
    /// Configured theme short name. Missing themes fall back to `default`.
    pub theme: String,
    /// Serve `*.br` / `*.gz` siblings when the client accepts them.
    pub precompressed: bool,
    pub immutable_max_age: u64,
    pub index_max_age: u64,
    pub asset_max_age: u64,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./data"),
            theme: DEFAULT_THEME.to_owned(),
            precompressed: true,
            immutable_max_age: 31_536_000,
            index_max_age: 0,
            asset_max_age: 300,
        }
    }
}

/// Values substituted into `dist/index.html`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteMeta {
    pub sitename: String,
    pub description: String,
    pub custom_head: String,
    pub custom_body: String,
}

impl Default for SiteMeta {
    fn default() -> Self {
        Self {
            sitename: "Komari Monitor".to_owned(),
            description: "A simple server monitor tool.".to_owned(),
            custom_head: String::new(),
            custom_body: String::new(),
        }
    }
}

/// Temporary share token from the `tempory_share_token` config keys.
///
/// The server layer loads these values from the database and calls
/// [`WebAssets::set_temp_share_token`]. The fallback then mirrors the Go
/// `temp_key` cookie behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempShareToken {
    pub token: String,
    /// Unix timestamp in seconds.
    pub expires_at: i64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ThemeSwitchError {
    #[error("invalid theme id: {0}")]
    InvalidId(String),
    #[error("theme is not installed or has no readable dist/index.html: {0}")]
    NotInstalled(String),
}

/// Shared static-asset service.
///
/// Clone the `Arc` into Axum state. The embedded theme is immutable; the
/// configured theme, site metadata and temporary share token can be updated at
/// runtime, matching the Go backend's live configuration reads.
#[derive(Debug)]
pub struct WebAssets {
    config: WebConfig,
    theme: RwLock<String>,
    site: RwLock<SiteMeta>,
    temp_share: RwLock<Option<TempShareToken>>,
}

impl WebAssets {
    pub fn new(config: WebConfig) -> Self {
        let theme = config.theme.clone();
        Self {
            config,
            theme: RwLock::new(theme),
            site: RwLock::new(SiteMeta::default()),
            temp_share: RwLock::new(None),
        }
    }

    pub fn with_data_dir(data_dir: impl Into<PathBuf>) -> Self {
        Self::new(WebConfig {
            data_dir: data_dir.into(),
            ..WebConfig::default()
        })
    }

    pub fn config(&self) -> &WebConfig {
        &self.config
    }

    pub fn theme(&self) -> String {
        self.theme
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Set the active theme without validating it.
    ///
    /// Prefer [`WebAssets::try_set_theme`] for user-facing theme switches. This
    /// unchecked setter remains useful when restoring configuration before the
    /// theme directory is mounted.
    pub fn set_theme(&self, theme: impl Into<String>) {
        *self
            .theme
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = theme.into();
    }

    /// Validate and switch the active theme.
    ///
    /// `default` is always available (it is embedded). Other themes must have a
    /// readable `dist/index.html` below `data/theme/<id>`.
    pub fn try_set_theme(&self, theme: impl Into<String>) -> Result<(), ThemeSwitchError> {
        let theme = theme.into();
        self.validate_theme(&theme)?;
        self.set_theme(theme);
        Ok(())
    }

    /// Check whether a theme can serve requests without switching to it.
    pub fn validate_theme(&self, theme: &str) -> Result<(), ThemeSwitchError> {
        if !is_safe_theme_id(theme) {
            return Err(ThemeSwitchError::InvalidId(theme.to_owned()));
        }
        if !self.is_theme_available(theme) {
            return Err(ThemeSwitchError::NotInstalled(theme.to_owned()));
        }
        Ok(())
    }

    /// Whether `theme` has a readable SPA entry point.
    ///
    /// This intentionally checks the directory contents, not
    /// `komari-theme.json`: Go serves any theme file that exists, and the
    /// metadata file is only needed for listings and theme settings.
    pub fn is_theme_available(&self, theme: &str) -> bool {
        if theme == DEFAULT_THEME {
            return true;
        }
        self.theme_root(theme)
            .is_some_and(|root| root.join(DIST_DIR).join(INDEX_FILE).is_file())
    }

    pub fn temp_share_token(&self) -> Option<TempShareToken> {
        self.temp_share
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn set_temp_share_token(&self, token: impl Into<String>, expires_at: i64) {
        *self
            .temp_share
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(TempShareToken {
            token: token.into(),
            expires_at,
        });
    }

    pub fn clear_temp_share_token(&self) {
        *self
            .temp_share
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    pub fn site_meta(&self) -> SiteMeta {
        self.site
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn set_site_meta(&self, meta: SiteMeta) {
        *self
            .site
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = meta;
    }

    /// The theme that will actually serve requests.
    ///
    /// A configured theme without a readable `dist/index.html` is ignored and
    /// `default` is used. This mirrors Go's directory-based serving: a theme
    /// does not need a valid `komari-theme.json` to be served, only its files.
    pub fn effective_theme(&self) -> String {
        let theme = self.theme();
        if self.is_theme_available(&theme) {
            theme
        } else {
            DEFAULT_THEME.to_owned()
        }
    }

    /// List the embedded default theme followed by valid themes in
    /// `data/theme/*`.
    pub fn list_themes(&self) -> Vec<ThemeInfo> {
        let themes_dir = self.config.data_dir.join(THEMES_DIR);
        let entries = match fs::read_dir(&themes_dir) {
            Ok(entries) => entries,
            // Go's ListThemes returns an empty list when data/theme is absent
            // (its init hook normally creates it). Match that observable
            // behaviour instead of advertising the embedded theme.
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(err) => {
                warn!(path = %themes_dir.display(), error = %err, "cannot read theme directory");
                return Vec::new();
            }
        };

        let mut themes = Vec::new();
        match DefaultThemeAssets::get(THEME_CONFIG_FILE) {
            Some(file) => match ThemeInfo::from_slice(&file.data) {
                Ok(info) => themes.push(info),
                Err(err) => warn!(error = %err, "embedded default theme config is invalid"),
            },
            None => warn!("embedded default theme has no komari-theme.json"),
        }

        let mut directories: Vec<_> = entries.filter_map(Result::ok).collect();
        directories.sort_by_key(|entry| entry.file_name());

        for entry in directories {
            let is_directory = entry
                .file_type()
                .map(|file_type| file_type.is_dir())
                .unwrap_or(false);
            if !is_directory {
                continue;
            }

            let config_path = entry.path().join(THEME_CONFIG_FILE);
            let data = match fs::read(&config_path) {
                Ok(data) => data,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => {
                    warn!(path = %config_path.display(), error = %err, "cannot read theme config");
                    continue;
                }
            };

            match ThemeInfo::from_slice(&data) {
                Ok(info) => themes.push(info),
                Err(err) => {
                    warn!(path = %config_path.display(), error = %err, "skipping invalid theme")
                }
            }
        }

        themes
    }

    /// Load metadata for one theme (`default` comes from the binary).
    pub fn theme_info(&self, theme: &str) -> Option<ThemeInfo> {
        if theme == DEFAULT_THEME {
            let file = DefaultThemeAssets::get(THEME_CONFIG_FILE)?;
            return ThemeInfo::from_slice(&file.data).ok();
        }

        let root = self.theme_root(theme)?;
        let data = fs::read(root.join(THEME_CONFIG_FILE)).ok()?;
        ThemeInfo::from_slice(&data).ok()
    }

    /// Resolve the preview image, trying the configured path first and then the
    /// `preview.png` / `perview.png` spellings.
    pub fn theme_preview(&self, theme: &str) -> Option<Asset> {
        let info = self.theme_info(theme)?;
        let candidates = info.preview_candidates();

        if theme == DEFAULT_THEME {
            for candidate in candidates {
                if let Some(asset) = self.resolve(theme, &candidate) {
                    return Some(asset);
                }
            }
            return None;
        }

        // Prefer files that belong to the theme itself. Falling back to the
        // embedded `preview.png` before trying a theme's `perview.png` would
        // show the default theme's preview for legacy packages.
        let root = self.theme_root(theme)?;
        let cache_control = CachePolicy::Asset.header_value(
            self.config.immutable_max_age,
            self.config.index_max_age,
            self.config.asset_max_age,
        );

        for candidate in candidates {
            if let Some(resolved) = read_disk_file(&root, &candidate) {
                return Some(Asset::new(
                    resolved.bytes,
                    &candidate,
                    cache_control,
                    None,
                    resolved.last_modified,
                ));
            }
        }
        None
    }

    /// Resolve a theme-relative file without content negotiation.
    pub fn resolve(&self, theme: &str, relative_path: &str) -> Option<Asset> {
        self.resolve_with_headers(theme, relative_path, None)
    }

    /// Resolve a theme-relative file, optionally selecting a pre-compressed
    /// variant based on `Accept-Encoding`.
    pub fn resolve_with_headers(
        &self,
        theme: &str,
        relative_path: &str,
        headers: Option<&HeaderMap>,
    ) -> Option<Asset> {
        let relative_path = sanitize_relative(relative_path)?;
        let resolved = self.resolve_identity(theme, &relative_path)?;
        let cache_control = CachePolicy::for_path(&relative_path).header_value(
            self.config.immutable_max_age,
            self.config.index_max_age,
            self.config.asset_max_age,
        );

        if self.config.precompressed {
            if let Some(headers) = headers {
                for coding in accepted_encodings(headers, &["br", "gzip"]) {
                    let extension = if coding == "br" { "br" } else { "gz" };
                    let variant_path = format!("{relative_path}.{extension}");
                    if let Some((bytes, last_modified)) =
                        self.read_from_source(&resolved.source, &variant_path)
                    {
                        return Some(Asset::new(
                            bytes,
                            &relative_path,
                            cache_control,
                            Some(coding),
                            last_modified,
                        ));
                    }
                }
            }
        }

        Some(Asset::new(
            resolved.bytes,
            &relative_path,
            cache_control,
            None,
            resolved.last_modified,
        ))
    }

    /// Render `dist/index.html`, applying site replacements unless `replace` is
    /// false (used for `/admin`).
    pub fn render_index(&self, theme: &str, replace: bool) -> Option<Asset> {
        let raw = self.resolve(theme, &format!("{DIST_DIR}/{INDEX_FILE}"))?;
        let bytes = if replace {
            let html = String::from_utf8_lossy(&raw.bytes);
            let meta = self.site_meta();
            Bytes::from(apply_site_replacements(&html, &meta).into_bytes())
        } else {
            raw.bytes
        };

        let cache_control = CachePolicy::Index.header_value(
            self.config.immutable_max_age,
            self.config.index_max_age,
            self.config.asset_max_age,
        );

        Some(Asset::new(
            bytes,
            &format!("{DIST_DIR}/{INDEX_FILE}"),
            cache_control,
            None,
            raw.last_modified,
        ))
    }

    fn theme_root(&self, theme: &str) -> Option<PathBuf> {
        if theme == DEFAULT_THEME || !is_safe_theme_id(theme) {
            return None;
        }
        Some(self.config.data_dir.join(THEMES_DIR).join(theme))
    }

    fn resolve_identity(&self, theme: &str, relative_path: &str) -> Option<ResolvedAsset> {
        if theme != DEFAULT_THEME {
            if let Some(root) = self.theme_root(theme) {
                if let Some(asset) = read_disk_file(&root, relative_path) {
                    return Some(asset);
                }
            }
        }

        let file = DefaultThemeAssets::get(relative_path)?;
        Some(ResolvedAsset {
            bytes: Bytes::copy_from_slice(&file.data),
            last_modified: file.metadata.last_modified().map(system_time_from_unix),
            source: AssetSource::Embedded,
        })
    }

    fn read_from_source(
        &self,
        source: &AssetSource,
        relative_path: &str,
    ) -> Option<(Bytes, Option<SystemTime>)> {
        let relative_path = sanitize_relative(relative_path)?;
        match source {
            AssetSource::Disk(root) => {
                let asset = read_disk_file(root, &relative_path)?;
                Some((asset.bytes, asset.last_modified))
            }
            AssetSource::Embedded => {
                let file = DefaultThemeAssets::get(&relative_path)?;
                Some((
                    Bytes::copy_from_slice(&file.data),
                    file.metadata.last_modified().map(system_time_from_unix),
                ))
            }
        }
    }
}

/// Build the static routes (`/favicon.ico`, `/themes/{id}/{*path}`) with state
/// already attached. Merge this into an application router and use
/// [`serve_fallback`] for the SPA fallback.
pub fn routes(assets: Arc<WebAssets>) -> Router {
    routes_for::<()>(assets)
}

/// Build a complete standalone router, including the SPA fallback. Useful for
/// tests and small deployments; applications with their own API routes should
/// use [`routes`] plus [`serve_fallback`].
pub fn router(assets: Arc<WebAssets>) -> Router {
    router_for::<()>(assets)
}

/// Build the static routes for an application with its own state type.
///
/// The handlers capture the shared [`WebAssets`] directly, so the returned
/// router can be merged into a `Router<AppState>` before that router gets its
/// state. This is the preferred integration point for the server crates.
pub fn routes_for<S>(assets: Arc<WebAssets>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let favicon_assets = assets.clone();
    let favicon = move |method: Method, headers: HeaderMap| {
        let assets = favicon_assets.clone();
        async move { serve_favicon(&assets, &method, &headers) }
    };

    let theme_assets = assets;
    let theme_asset = move |AxumPath((theme, path)): AxumPath<(String, String)>,
                            method: Method,
                            headers: HeaderMap| {
        let assets = theme_assets.clone();
        async move {
            let theme = percent_decode_str(&theme).decode_utf8_lossy();
            let path = percent_decode_str(&path).decode_utf8_lossy();
            serve_theme_asset(&assets, &theme, &path, &method, &headers)
        }
    };

    Router::new()
        .route("/favicon.ico", any(favicon))
        .route("/themes/{id}/{*path}", any(theme_asset))
}

/// Like [`routes_for`], but also installs the SPA fallback.
pub fn router_for<S>(assets: Arc<WebAssets>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let fallback_assets = assets.clone();
    let fallback = move |request: Request| {
        let assets = fallback_assets.clone();
        async move { serve_fallback(&assets, request).await }
    };

    routes_for(assets).fallback(fallback)
}

/// Serve `data/favicon.ico`, then the active theme's `dist/favicon.ico`, then
/// the embedded default. Returns 404 when neither exists.
pub fn serve_favicon(assets: &WebAssets, method: &Method, headers: &HeaderMap) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return not_found();
    }

    let local_path = assets.config().data_dir.join(FAVICON_FILE);
    if let Ok(metadata) = fs::metadata(&local_path) {
        if metadata.is_file() {
            if let Ok(bytes) = fs::read(&local_path) {
                let cache_control = CachePolicy::Asset.header_value(
                    assets.config().immutable_max_age,
                    assets.config().index_max_age,
                    assets.config().asset_max_age,
                );
                let asset = Asset::new(
                    Bytes::from(bytes),
                    FAVICON_FILE,
                    cache_control,
                    None,
                    metadata.modified().ok(),
                );
                return asset.into_response(method, headers);
            }
        }
    }

    let theme = assets.effective_theme();
    if let Some(asset) =
        assets.resolve_with_headers(&theme, &format!("{DIST_DIR}/{FAVICON_FILE}"), Some(headers))
    {
        return asset.into_response(method, headers);
    }

    not_found()
}

/// Serve a file below `/themes/{id}/...`.
pub fn serve_theme_asset(
    assets: &WebAssets,
    theme: &str,
    path: &str,
    method: &Method,
    headers: &HeaderMap,
) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return not_found();
    }
    if !is_safe_theme_id(theme) || has_traversal(path) {
        return not_found();
    }

    match assets.resolve_with_headers(theme, path, Some(headers)) {
        Some(asset) => asset.into_response(method, headers),
        None => not_found(),
    }
}

/// SPA fallback handler.
///
/// `GET`/`HEAD` requests under `/api` return 404 (never the SPA). For every
/// other unknown path the active theme's `dist` directory is tried first and
/// `index.html` is returned last.
pub async fn serve_fallback(assets: &WebAssets, request: Request) -> Response {
    // Go's noRoute sets the temporary-share cookie before dispatching to the
    // SPA/static handler, including for paths that end up as 404. Non-GET
    // requests are rejected before that block, so they never set a cookie.
    let cookie = if request.method() == Method::GET || request.method() == Method::HEAD {
        temp_key_cookie(assets, request.uri().query())
    } else {
        None
    };
    let mut response = fallback_response(assets, request).await;
    if let Some(cookie) = cookie {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    response
}

async fn fallback_response(assets: &WebAssets, request: Request) -> Response {
    let method = request.method().clone();
    if method != Method::GET && method != Method::HEAD {
        return not_found();
    }

    let decoded_path = percent_decode_str(request.uri().path()).decode_utf8_lossy();
    let path = decoded_path.as_ref();

    if path == "/api" || path.starts_with("/api/") {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::empty())
            .expect("valid 404 response");
    }

    // The router may not match an encoded `..%2f` sequence, so reject
    // traversal attempts here before they can fall through to `index.html`.
    if has_traversal(path) {
        return not_found();
    }

    let theme = assets.effective_theme();
    let relative_path = format!("{DIST_DIR}{path}");
    if let Some(asset) =
        assets.resolve_with_headers(&theme, &relative_path, Some(request.headers()))
    {
        return asset.into_response(&method, request.headers());
    }

    let is_admin = path.starts_with("/admin");
    let (index_theme, replace) = if is_admin {
        (DEFAULT_THEME, false)
    } else {
        (theme.as_str(), true)
    };

    match assets.render_index(index_theme, replace) {
        Some(asset) => asset.into_response(&method, request.headers()),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(Body::from(format!(
                "Index file missing (checked {index_theme}/dist/index.html and default)."
            )))
            .expect("valid 404 response"),
    }
}

#[derive(Debug)]
struct ResolvedAsset {
    bytes: Bytes,
    last_modified: Option<SystemTime>,
    source: AssetSource,
}

#[derive(Debug)]
enum AssetSource {
    Disk(PathBuf),
    Embedded,
}

fn read_disk_file(root: &Path, relative_path: &str) -> Option<ResolvedAsset> {
    let relative_path = sanitize_relative(relative_path)?;
    let root = root.canonicalize().ok()?;
    let path = root.join(&relative_path);
    let canonical = path.canonicalize().ok()?;

    if !canonical.starts_with(&root) {
        return None;
    }

    let metadata = fs::metadata(&canonical).ok()?;
    if !metadata.is_file() {
        return None;
    }

    let bytes = fs::read(&canonical).ok()?;
    Some(ResolvedAsset {
        bytes: Bytes::from(bytes),
        last_modified: metadata.modified().ok(),
        source: AssetSource::Disk(root),
    })
}

/// Normalize a theme-relative path and reject traversal.
///
/// Returns `None` for absolute paths, `..` components, backslashes and NUL
/// bytes. Empty and `.` components are dropped.
pub fn sanitize_relative(path: &str) -> Option<String> {
    if path.contains('\0') || path.contains('\\') {
        return None;
    }

    let mut components = Vec::new();
    for component in path.trim_start_matches('/').split('/') {
        match component {
            "" | "." => {}
            ".." => return None,
            component => components.push(component),
        }
    }

    if components.is_empty() {
        None
    } else {
        Some(components.join("/"))
    }
}

/// Theme ids are directory names: reject separators, traversal and control
/// characters. `default` is handled by the caller.
pub fn is_safe_theme_id(theme: &str) -> bool {
    !theme.is_empty()
        && theme != "."
        && theme != ".."
        && !theme.contains('/')
        && !theme.contains('\\')
        && !theme.contains('\0')
        && theme.chars().all(|character| !character.is_control())
}

fn temp_key_cookie(assets: &WebAssets, query: Option<&str>) -> Option<HeaderValue> {
    let query = query?;
    let temp_key = form_urlencoded::parse(query.as_bytes())
        .find_map(|(key, value)| (key == "temp_key").then(|| value.into_owned()))?;
    if temp_key.is_empty() {
        return None;
    }

    let share = assets.temp_share_token()?;
    if share.token.is_empty() || temp_key != share.token {
        return None;
    }

    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    let max_age = share.expires_at - now;
    if max_age <= 0 {
        return None;
    }

    // Keep the cookie header valid without silently dropping characters the
    // way Go's net/http sanitizer does.
    if !share.token.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '~')
    }) {
        return None;
    }

    HeaderValue::from_str(&format!(
        "temp_key={}; Path=/; Max-Age={max_age}",
        share.token
    ))
    .ok()
}

fn has_traversal(path: &str) -> bool {
    path.contains('\\') || path.contains('\0') || path.split('/').any(|component| component == "..")
}

fn not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::empty())
        .expect("valid 404 response")
}
