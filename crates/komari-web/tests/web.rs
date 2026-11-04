//! Integration and unit tests for the static asset service.
//!
//! The tests deliberately avoid depending on the contents or file names of the
//! embedded default theme. They build temporary themes with known files, so the
//! same suite passes with the committed placeholder and with a real frontend
//! build (`KOMARI_DEFAULT_THEME_DIR=/path/to/komari-web`).

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use bytes::Bytes;
use http_body_util::BodyExt;
use komari_web::{
    SiteMeta, ThemeInfo, ThemeLayoutError, ThemeSwitchError, WebAssets, WebConfig,
    accepted_encodings, apply_site_replacements, content_type_for, encoding_quality,
    is_safe_theme_id, router, router_for, routes_for, sanitize_relative, validate_theme_layout,
};
use tempfile::TempDir;
use tower::ServiceExt;

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Bytes) {
    let response = app.clone().oneshot(request).await.expect("request");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, headers, body)
}

fn get(path: &str) -> Request<Body> {
    request(Method::GET, path)
}

fn request(method: Method, path: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .expect("request")
}

fn get_with_encoding(path: &str, encoding: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(path)
        .header(header::ACCEPT_ENCODING, encoding)
        .body(Body::empty())
        .expect("request")
}

fn test_assets(data_dir: &Path) -> Arc<WebAssets> {
    Arc::new(WebAssets::with_data_dir(data_dir))
}

fn test_app(data_dir: &Path) -> Router {
    router(test_assets(data_dir))
}

fn theme_assets(data_dir: &Path, theme: &str) -> Arc<WebAssets> {
    Arc::new(WebAssets::new(WebConfig {
        data_dir: data_dir.to_path_buf(),
        theme: theme.to_owned(),
        ..WebConfig::default()
    }))
}

fn write_theme(root: &Path, short: &str, index_html: &str) -> PathBuf {
    let dir = root.join("theme").join(short);
    fs::create_dir_all(dir.join("dist/assets")).expect("theme dir");
    fs::write(
        dir.join("komari-theme.json"),
        format!(
            r#"{{"name":"{short}","short":"{short}","description":"test theme","version":"1.2.3","author":"tester","preview":"preview.png"}}"#
        ),
    )
    .expect("theme config");
    fs::write(dir.join("dist/index.html"), index_html).expect("index");
    fs::write(dir.join("preview.png"), b"\x89PNG\r\n\x1a\npreview").expect("preview");
    dir
}

fn write_theme_asset(theme: &Path, relative_path: &str, bytes: &[u8]) {
    let path = theme.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("asset parent");
    }
    fs::write(path, bytes).expect("asset");
}

fn tempdir() -> TempDir {
    // Cargo points this at `target/tmp`, which avoids depending on `/tmp`
    // capacity (important in constrained CI containers).
    let base = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    fs::create_dir_all(&base).expect("temp base");
    tempfile::Builder::new()
        .prefix("komari-web-")
        .tempdir_in(base)
        .expect("tempdir")
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}

fn header_str(headers: &HeaderMap, name: axum::http::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

#[tokio::test]
async fn serves_embedded_index_with_short_cache() {
    let data = tempdir();
    let app = test_app(data.path());

    let (status, headers, body) = send(&app, get("/")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        Some("text/html; charset=utf-8")
    );
    let cache = header_str(&headers, header::CACHE_CONTROL).unwrap_or_default();
    assert!(cache.contains("max-age=0"), "cache was {cache}");
    assert!(
        String::from_utf8_lossy(&body)
            .to_ascii_lowercase()
            .contains("<!doctype html>"),
        "embedded index did not look like an HTML document"
    );
}

#[tokio::test]
async fn serves_theme_asset_with_long_cache_and_javascript_type() {
    let data = tempdir();
    let theme = write_theme(data.path(), "acme", "<html></html>");
    write_theme_asset(
        &theme,
        "dist/assets/index-abc12345.js",
        b"console.log('acme');",
    );

    let app = router(theme_assets(data.path(), "acme"));
    let (status, headers, body) = send(&app, get("/assets/index-abc12345.js")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        Some("text/javascript; charset=utf-8")
    );
    let cache = header_str(&headers, header::CACHE_CONTROL).unwrap_or_default();
    assert!(cache.contains("max-age=31536000"), "cache was {cache}");
    assert!(cache.contains("immutable"), "cache was {cache}");
    assert_eq!(body.as_ref(), b"console.log('acme');");
}

#[tokio::test]
async fn spa_route_falls_back_to_theme_index() {
    let data = tempdir();
    write_theme(
        data.path(),
        "acme",
        "<!DOCTYPE html><html><body>ACME-SPA-INDEX</body></html>",
    );

    let app = router(theme_assets(data.path(), "acme"));
    let (status, headers, body) = send(&app, get("/dashboard/overview")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        Some("text/html; charset=utf-8")
    );
    assert!(String::from_utf8_lossy(&body).contains("ACME-SPA-INDEX"));
}

#[tokio::test]
async fn api_routes_never_fall_back_to_the_spa() {
    let data = tempdir();
    let app = test_app(data.path());

    let (status, headers, body) = send(&app, get("/api/does-not-exist")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        Some("no-store")
    );
    assert!(body.is_empty());

    let (status, _, body) = send(&app, get("/api")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty());

    let (status, _, body) = send(&app, request(Method::POST, "/api/unknown")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty());
}

#[tokio::test]
async fn blocks_path_traversal() {
    let data = tempdir();
    let app = test_app(data.path());

    for path in [
        "/themes/default/..%2f..%2fCargo.toml",
        "/themes/..%2f..%2fetc/dist/index.html",
        "/themes/default/%2e%2e/%2e%2e/Cargo.toml",
        "/..%2fCargo.toml",
    ] {
        let (status, _, body) = send(&app, get(path)).await;
        assert_ne!(status, StatusCode::OK, "path {path} unexpectedly succeeded");
        assert!(
            !String::from_utf8_lossy(&body).contains("[workspace]"),
            "path {path} leaked the workspace manifest"
        );
    }

    assert_eq!(sanitize_relative("../etc/passwd"), None);
    assert_eq!(sanitize_relative("a/../../b"), None);
    assert_eq!(sanitize_relative("a\\b"), None);
    assert_eq!(
        sanitize_relative("/dist/index.html").as_deref(),
        Some("dist/index.html")
    );
    assert!(!is_safe_theme_id("../default"));
    assert!(!is_safe_theme_id("a/b"));
    assert!(!is_safe_theme_id(""));
    assert!(is_safe_theme_id("my-theme_1"));
}

#[tokio::test]
async fn missing_theme_falls_back_to_default() {
    let data = tempdir();
    let assets = theme_assets(data.path(), "missing-theme");
    let app = router(assets.clone());

    assert_eq!(assets.effective_theme(), "default");

    let (status, _, body) = send(&app, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        String::from_utf8_lossy(&body)
            .to_ascii_lowercase()
            .contains("<!doctype html>")
    );
}

#[tokio::test]
async fn theme_without_metadata_is_still_served() {
    let data = tempdir();
    let dist = data.path().join("theme/raw/dist");
    fs::create_dir_all(&dist).expect("raw theme");
    fs::write(
        dist.join("index.html"),
        "<html><body>RAW-THEME</body></html>",
    )
    .expect("index");

    let assets = theme_assets(data.path(), "raw");
    assert_eq!(assets.effective_theme(), "raw");

    let app = router(assets);
    let (status, _, body) = send(&app, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("RAW-THEME"));
}

#[tokio::test]
async fn custom_theme_overrides_default_and_falls_back_per_file() {
    let data = tempdir();
    write_theme(
        data.path(),
        "acme",
        "<html><head><title>Komari Monitor</title></head><body>ACME</body></html>",
    );

    let app = router(theme_assets(data.path(), "acme"));

    let (status, _, body) = send(&app, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("ACME"));

    let (status, headers, body) = send(&app, get("/themes/acme/dist/index.html")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        Some("text/html; charset=utf-8")
    );
    assert!(String::from_utf8_lossy(&body).contains("ACME"));

    // A file that only exists in the embedded default theme is still served.
    let (status, headers, body) = send(&app, get("/themes/acme/dist/favicon.ico")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        Some("image/vnd.microsoft.icon")
    );
    assert!(!body.is_empty());
}

#[tokio::test]
async fn lists_themes_and_skips_invalid_configs() {
    let data = tempdir();
    write_theme(data.path(), "acme", "<html></html>");

    let broken = data.path().join("theme/broken");
    fs::create_dir_all(broken.join("dist")).expect("broken dir");
    fs::write(
        broken.join("komari-theme.json"),
        r#"{"short":"broken","version":"1.0.0"}"#,
    )
    .expect("broken config");

    let garbage = data.path().join("theme/garbage");
    fs::create_dir_all(&garbage).expect("garbage dir");
    fs::write(garbage.join("komari-theme.json"), "not json").expect("garbage config");

    let assets = WebAssets::with_data_dir(data.path());
    let themes = assets.list_themes();

    assert_eq!(themes.len(), 2, "themes were {themes:?}");
    assert_eq!(themes[0].short, "default");
    assert!(!themes[0].name.is_empty());
    assert!(!themes[0].preview.is_empty());
    assert_eq!(themes[1].short, "acme");
    assert_eq!(themes[1].description, "test theme");
    assert_eq!(themes[1].version, "1.2.3");
    assert_eq!(themes[1].author, "tester");
    assert_eq!(themes[1].preview, "preview.png");
}

#[test]
fn list_themes_returns_empty_without_theme_directory() {
    let data = tempdir();
    let assets = WebAssets::with_data_dir(data.path());
    assert!(assets.list_themes().is_empty());
}

#[test]
fn theme_config_requires_name_and_short() {
    let valid =
        ThemeInfo::from_json(r#"{"name":"Theme","short":"theme","version":"1.0.0","author":"me"}"#)
            .expect("valid theme");
    assert_eq!(valid.short, "theme");
    assert_eq!(
        valid.configuration,
        komari_web::ThemeConfiguration::default()
    );

    let missing_name = ThemeInfo::from_json(r#"{"short":"theme"}"#).expect_err("missing name");
    assert!(missing_name.to_string().contains("name"));

    let missing_short = ThemeInfo::from_json(r#"{"name":"Theme"}"#).expect_err("missing short");
    assert!(missing_short.to_string().contains("short"));

    let invalid = ThemeInfo::from_json("{").expect_err("invalid json");
    assert!(invalid.to_string().contains("invalid theme JSON"));
}

#[tokio::test]
async fn preview_images_support_both_spellings() {
    let data = tempdir();
    let theme = write_theme(data.path(), "acme", "<html></html>");
    fs::write(theme.join("preview.png"), b"png-preview").expect("preview");
    fs::write(theme.join("perview.png"), b"png-perview").expect("perview");

    let app = router(theme_assets(data.path(), "acme"));

    let (status, headers, body) = send(&app, get("/themes/acme/preview.png")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        Some("image/png")
    );
    assert_eq!(body.as_ref(), b"png-preview");

    let (status, headers, body) = send(&app, get("/themes/acme/perview.png")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        Some("image/png")
    );
    assert_eq!(body.as_ref(), b"png-perview");

    // A theme that only ships the historical `perview.png` spelling still gets
    // a preview through the fallback list.
    let legacy = write_theme(data.path(), "legacy", "<html></html>");
    fs::remove_file(legacy.join("preview.png")).expect("remove preview");
    fs::write(legacy.join("perview.png"), b"\x89PNG\r\n\x1a\nlegacy").expect("perview");
    fs::write(
        legacy.join("komari-theme.json"),
        r#"{"name":"Legacy","short":"legacy","preview":""}"#,
    )
    .expect("legacy config");

    let assets = theme_assets(data.path(), "legacy");
    let preview = assets.theme_preview("legacy").expect("legacy preview");
    assert_eq!(preview.content_type, "image/png");
    assert_eq!(preview.bytes.as_ref(), b"\x89PNG\r\n\x1a\nlegacy");
}

#[tokio::test]
async fn etag_revalidation_returns_304() {
    let data = tempdir();
    let theme = write_theme(data.path(), "acme", "<html></html>");
    write_theme_asset(&theme, "dist/assets/app-abc12345.css", b"body{color:red}");

    let app = router(theme_assets(data.path(), "acme"));
    let (status, headers, body) = send(&app, get("/assets/app-abc12345.css")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        Some("text/css; charset=utf-8")
    );
    assert_eq!(body.as_ref(), b"body{color:red}");
    let etag = header_str(&headers, header::ETAG).expect("etag").to_owned();

    let request = Request::builder()
        .uri("/assets/app-abc12345.css")
        .header(header::IF_NONE_MATCH, &etag)
        .body(Body::empty())
        .expect("request");
    let (status, headers, body) = send(&app, request).await;

    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert_eq!(header_str(&headers, header::ETAG), Some(etag.as_str()));
    assert!(headers.contains_key(header::CACHE_CONTROL));
    assert!(body.is_empty());
}

#[tokio::test]
async fn head_requests_return_headers_without_a_body() {
    let data = tempdir();
    let theme = write_theme(data.path(), "acme", "<html></html>");
    let payload = b"console.log('head');";
    write_theme_asset(&theme, "dist/assets/app-abc12345.js", payload);

    let app = router(theme_assets(data.path(), "acme"));
    let (status, headers, body) =
        send(&app, request(Method::HEAD, "/assets/app-abc12345.js")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        Some("text/javascript; charset=utf-8")
    );
    assert_eq!(
        header_str(&headers, header::CONTENT_LENGTH).and_then(|value| value.parse::<usize>().ok()),
        Some(payload.len())
    );
}

#[tokio::test]
async fn index_replacements_apply_and_admin_is_untouched() {
    let data = tempdir();
    write_theme(
        data.path(),
        "acme",
        "<!DOCTYPE html><html><head><meta name=\"description\" content=\"A simple server monitor tool.\"><title>Komari Monitor</title></head><body>ACME-INDEX</body></html>",
    );

    let assets = theme_assets(data.path(), "acme");
    assets.set_site_meta(SiteMeta {
        sitename: "My Site".to_owned(),
        description: "Custom description".to_owned(),
        custom_head: "<meta name=\"custom\" content=\"1\">".to_owned(),
        custom_body: "<footer>custom</footer>".to_owned(),
    });
    let app = router(assets);

    let (status, _, body) = send(&app, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("<title>My Site</title>"));
    assert!(html.contains("Custom description"));
    assert!(html.contains("<meta name=\"custom\" content=\"1\"></head>"));
    assert!(html.contains("<footer>custom</footer></body>"));
    assert!(html.contains("ACME-INDEX"));

    // `/admin` always uses the embedded default theme and never rewrites it.
    let (status, _, body) = send(&app, get("/admin/dashboard")).await;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8_lossy(&body);
    assert!(html.to_ascii_lowercase().contains("<!doctype html>"));
    assert!(!html.contains("ACME-INDEX"));
    assert!(!html.contains("My Site"));
    assert!(!html.contains("Custom description"));
    assert!(!html.contains("name=\"custom\""));
}

#[test]
fn html_replacements_are_single_pass() {
    let meta = SiteMeta {
        sitename: "Site".to_owned(),
        description: "desc </body> injected".to_owned(),
        custom_head: "</head>".to_owned(),
        custom_body: "</body>".to_owned(),
    };
    let input =
        "<head><title>Komari Monitor</title>A simple server monitor tool.</head><body>x</body>";
    let output = apply_site_replacements(input, &meta);

    assert!(output.contains("<title>Site</title>"));
    assert!(output.contains("desc </body> injected"));
    // The `</body>` that came from the description must not be expanded again.
    assert!(output.contains("injected</head></head><body>x</body></body>"));
}

#[tokio::test]
async fn favicon_prefers_data_directory() {
    let data = tempdir();
    let app = test_app(data.path());

    let (status, headers, body) = send(&app, get("/favicon.ico")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        Some("image/vnd.microsoft.icon")
    );
    assert!(!body.is_empty());

    fs::write(data.path().join("favicon.ico"), b"custom-favicon").expect("favicon");
    let (status, _, body) = send(&app, get("/favicon.ico")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), b"custom-favicon");
}

#[tokio::test]
async fn precompressed_variants_are_optional() {
    let data = tempdir();
    let theme = write_theme(data.path(), "acme", "<html></html>");
    write_theme_asset(&theme, "dist/assets/app-abc12345.js", b"identity");
    write_theme_asset(&theme, "dist/assets/app-abc12345.js.gz", b"gzip-bytes");
    write_theme_asset(&theme, "dist/assets/app-abc12345.js.br", b"brotli-bytes");

    let app = router(theme_assets(data.path(), "acme"));

    let (status, headers, body) =
        send(&app, get_with_encoding("/assets/app-abc12345.js", "gzip")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_str(&headers, header::CONTENT_ENCODING), Some("gzip"));
    assert_eq!(header_str(&headers, header::VARY), Some("Accept-Encoding"));
    assert_eq!(body.as_ref(), b"gzip-bytes");

    let (status, headers, body) =
        send(&app, get_with_encoding("/assets/app-abc12345.js", "br")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_str(&headers, header::CONTENT_ENCODING), Some("br"));
    assert_eq!(body.as_ref(), b"brotli-bytes");

    let (status, headers, body) = send(&app, get("/assets/app-abc12345.js")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::CONTENT_ENCODING).is_none());
    assert_eq!(header_str(&headers, header::VARY), Some("Accept-Encoding"));
    assert_eq!(body.as_ref(), b"identity");

    // Disabling the option always serves identity.
    let plain = Arc::new(WebAssets::new(WebConfig {
        data_dir: data.path().to_path_buf(),
        theme: "acme".to_owned(),
        precompressed: false,
        ..WebConfig::default()
    }));
    let plain_app = router(plain);
    let (status, headers, body) = send(
        &plain_app,
        get_with_encoding("/assets/app-abc12345.js", "gzip"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::CONTENT_ENCODING).is_none());
    assert_eq!(body.as_ref(), b"identity");
}

#[tokio::test]
async fn precompressed_negotiates_quality_and_wildcard() {
    let data = tempdir();
    let theme = write_theme(data.path(), "acme", "<html></html>");
    write_theme_asset(&theme, "dist/assets/app-abc12345.js", b"identity");
    write_theme_asset(&theme, "dist/assets/app-abc12345.js.gz", b"gzip-bytes");
    write_theme_asset(&theme, "dist/assets/app-abc12345.js.br", b"brotli-bytes");

    let app = router(theme_assets(data.path(), "acme"));

    let (_, headers, body) = send(
        &app,
        get_with_encoding("/assets/app-abc12345.js", "gzip;q=0.5, br;q=0.9"),
    )
    .await;
    assert_eq!(header_str(&headers, header::CONTENT_ENCODING), Some("br"));
    assert_eq!(body.as_ref(), b"brotli-bytes");

    let (_, headers, body) = send(
        &app,
        get_with_encoding("/assets/app-abc12345.js", "gzip;q=0.9, br;q=0.1"),
    )
    .await;
    assert_eq!(header_str(&headers, header::CONTENT_ENCODING), Some("gzip"));
    assert_eq!(body.as_ref(), b"gzip-bytes");

    let (_, headers, _) = send(&app, get_with_encoding("/assets/app-abc12345.js", "*")).await;
    assert_eq!(header_str(&headers, header::CONTENT_ENCODING), Some("br"));

    let (_, headers, body) = send(
        &app,
        get_with_encoding("/assets/app-abc12345.js", "gzip;q=0"),
    )
    .await;
    assert!(headers.get(header::CONTENT_ENCODING).is_none());
    assert_eq!(body.as_ref(), b"identity");

    // If the preferred variant is missing, try the next accepted coding.
    let gz_only = write_theme(data.path(), "gz-only", "<html></html>");
    write_theme_asset(&gz_only, "dist/assets/app-abc12345.js", b"identity");
    write_theme_asset(&gz_only, "dist/assets/app-abc12345.js.gz", b"gzip-bytes");
    let gz_app = router(theme_assets(data.path(), "gz-only"));
    let (_, headers, body) = send(
        &gz_app,
        get_with_encoding("/assets/app-abc12345.js", "br;q=1, gzip;q=0.5"),
    )
    .await;
    assert_eq!(header_str(&headers, header::CONTENT_ENCODING), Some("gzip"));
    assert_eq!(body.as_ref(), b"gzip-bytes");

    assert_eq!(
        accepted_encodings(
            &{
                let mut headers = HeaderMap::new();
                headers.insert(
                    header::ACCEPT_ENCODING,
                    "gzip;q=0.5, br;q=0.9".parse().expect("header"),
                );
                headers
            },
            &["br", "gzip"],
        ),
        vec!["br", "gzip"]
    );
    let mut wildcard = HeaderMap::new();
    wildcard.insert(header::ACCEPT_ENCODING, "*;q=0.4".parse().expect("header"));
    assert_eq!(encoding_quality(&wildcard, "br"), Some(0.4));
}

#[test]
fn theme_layout_reports_missing_parts() {
    let missing = tempdir();
    assert!(matches!(
        validate_theme_layout(missing.path().join("nope")),
        Err(ThemeLayoutError::MissingDirectory(_))
    ));

    let no_dist = tempdir();
    assert!(matches!(
        validate_theme_layout(no_dist.path()),
        Err(ThemeLayoutError::MissingDist(_))
    ));

    let no_index = tempdir();
    fs::create_dir(no_index.path().join("dist")).expect("dist");
    fs::write(no_index.path().join("komari-theme.json"), "{}").expect("config");
    fs::write(no_index.path().join("preview.png"), b"png").expect("preview");
    assert!(matches!(
        validate_theme_layout(no_index.path()),
        Err(ThemeLayoutError::MissingIndex(_))
    ));

    let no_preview = tempdir();
    fs::create_dir(no_preview.path().join("dist")).expect("dist");
    fs::write(no_preview.path().join("dist/index.html"), "html").expect("index");
    fs::write(no_preview.path().join("komari-theme.json"), "{}").expect("config");
    assert!(matches!(
        validate_theme_layout(no_preview.path()),
        Err(ThemeLayoutError::MissingPreview(_))
    ));

    let complete = tempdir();
    fs::create_dir(complete.path().join("dist")).expect("dist");
    fs::write(complete.path().join("dist/index.html"), "html").expect("index");
    fs::write(complete.path().join("komari-theme.json"), "{}").expect("config");
    fs::write(complete.path().join("perview.png"), b"png").expect("preview");
    let layout = validate_theme_layout(complete.path()).expect("valid layout");
    assert!(layout.preview.ends_with("perview.png"));
}

#[test]
fn content_types_match_go_builtins() {
    assert_eq!(
        content_type_for("dist/index.html"),
        "text/html; charset=utf-8"
    );
    assert_eq!(
        content_type_for("dist/assets/app.js"),
        "text/javascript; charset=utf-8"
    );
    assert_eq!(
        content_type_for("dist/assets/app.css"),
        "text/css; charset=utf-8"
    );
    assert_eq!(content_type_for("dist/manifest.json"), "application/json");
    assert_eq!(
        content_type_for("dist/assets/data.xml"),
        "text/xml; charset=utf-8"
    );
    assert_eq!(
        content_type_for("dist/favicon.ico"),
        "image/vnd.microsoft.icon"
    );
    assert_eq!(content_type_for("dist/assets/font.woff2"), "font/woff2");
    assert_eq!(content_type_for("dist/assets/logo.svg"), "image/svg+xml");
    assert_eq!(
        content_type_for("dist/assets/unknown.bin"),
        "application/octet-stream"
    );
}

#[test]
fn routes_compose_with_an_application_state() {
    #[derive(Clone)]
    struct AppState;

    let data = tempdir();
    let assets = test_assets(data.path());

    let _: Router<AppState> = routes_for::<AppState>(assets.clone());
    let _: Router<AppState> = router_for::<AppState>(assets);
}

#[tokio::test]
async fn temp_key_sets_cookie_for_valid_share() {
    let data = tempdir();
    let assets = test_assets(data.path());
    let app = router(assets.clone());

    assets.set_temp_share_token("secret-token", now_unix() + 120);
    let (status, headers, _) = send(&app, get("/share?temp_key=secret-token")).await;
    assert_eq!(status, StatusCode::OK);
    let cookie = header_str(&headers, header::SET_COOKIE).expect("set-cookie");
    assert!(cookie.contains("temp_key=secret-token"));
    assert!(cookie.contains("Path=/"));
    assert!(cookie.contains("Max-Age="));

    let (status, headers, _) = send(&app, get("/share?temp_key=wrong")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::SET_COOKIE).is_none());

    // Go sets the cookie before dispatching, including for `/api` 404s.
    let (status, headers, _) = send(&app, get("/api/unknown?temp_key=secret-token")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(headers.get(header::SET_COOKIE).is_some());

    // Non-GET requests are rejected before the cookie block in Go.
    let (status, headers, _) =
        send(&app, request(Method::POST, "/share?temp_key=secret-token")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(headers.get(header::SET_COOKIE).is_none());

    assets.set_temp_share_token("secret-token", now_unix() - 1);
    let (_, headers, _) = send(&app, get("/share?temp_key=secret-token")).await;
    assert!(headers.get(header::SET_COOKIE).is_none());

    assets.clear_temp_share_token();
    let (_, headers, _) = send(&app, get("/share?temp_key=secret-token")).await;
    assert!(headers.get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn non_get_static_routes_return_404() {
    let data = tempdir();
    let app = test_app(data.path());

    let (status, _, body) = send(&app, request(Method::POST, "/favicon.ico")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty());

    let (status, _, body) = send(&app, request(Method::PUT, "/themes/default/preview.png")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty());
}

#[tokio::test]
async fn theme_switch_validates_and_takes_effect() {
    let data = tempdir();
    write_theme(
        data.path(),
        "acme",
        "<!DOCTYPE html><html><body>ACME-SWITCH</body></html>",
    );

    let assets = test_assets(data.path());
    let app = router(assets.clone());

    let (_, _, body) = send(&app, get("/")).await;
    assert!(!String::from_utf8_lossy(&body).contains("ACME-SWITCH"));

    assets.try_set_theme("acme").expect("switch to acme");
    let (status, _, body) = send(&app, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("ACME-SWITCH"));

    assert!(matches!(
        assets.try_set_theme("missing"),
        Err(ThemeSwitchError::NotInstalled(_))
    ));
    assert!(matches!(
        assets.try_set_theme("../acme"),
        Err(ThemeSwitchError::InvalidId(_))
    ));
    assert!(assets.validate_theme("default").is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_theme_file_is_rejected() {
    use std::os::unix::fs::symlink;

    let data = tempdir();
    let theme = write_theme(data.path(), "acme", "<html>ACME</html>");
    let outside = data.path().join("outside-secret.txt");
    fs::write(&outside, b"outside-secret").expect("outside file");
    symlink(&outside, theme.join("dist/secret.txt")).expect("symlink");

    let app = router(theme_assets(data.path(), "acme"));
    let (status, _, body) = send(&app, get("/themes/acme/dist/secret.txt")).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!String::from_utf8_lossy(&body).contains("outside-secret"));
}

#[tokio::test]
async fn placeholder_documentation_is_not_embedded() {
    let data = tempdir();
    let app = test_app(data.path());

    for path in [
        "/themes/default/PLACEHOLDER.md",
        "/themes/default/PLACEHOLDER_THEME.md",
    ] {
        let (status, _, _) = send(&app, get(path)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "path {path}");
    }
}
