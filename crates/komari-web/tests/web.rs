//! Integration and unit tests for the static asset service.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use bytes::Bytes;
use http_body_util::BodyExt;
use komari_web::{
    SiteMeta, ThemeInfo, ThemeLayoutError, WebAssets, WebConfig, apply_site_replacements,
    content_type_for, is_safe_theme_id, router, router_for, routes_for, sanitize_relative,
    validate_theme_layout,
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
    Request::builder()
        .method(Method::GET)
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

#[tokio::test]
async fn serves_embedded_index_with_short_cache() {
    let data = TempDir::new().expect("tempdir");
    let app = test_app(data.path());

    let (status, headers, body) = send(&app, get("/")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    let cache = headers
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(cache.contains("max-age=0"), "cache was {cache}");
    assert!(String::from_utf8_lossy(&body).contains(r#"id="root""#));
}

#[tokio::test]
async fn serves_hashed_asset_with_long_cache_and_javascript_type() {
    let data = TempDir::new().expect("tempdir");
    let app = test_app(data.path());

    let (status, headers, body) = send(&app, get("/assets/index-CVMackoC.js")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/javascript; charset=utf-8")
    );
    let cache = headers
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(cache.contains("max-age=31536000"), "cache was {cache}");
    assert!(cache.contains("immutable"), "cache was {cache}");
    assert!(String::from_utf8_lossy(&body).contains("placeholder"));
}

#[tokio::test]
async fn spa_route_falls_back_to_index() {
    let data = TempDir::new().expect("tempdir");
    let app = test_app(data.path());

    let (status, headers, body) = send(&app, get("/dashboard/overview")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert!(String::from_utf8_lossy(&body).contains(r#"id="root""#));
}

#[tokio::test]
async fn api_routes_never_fall_back_to_the_spa() {
    let data = TempDir::new().expect("tempdir");
    let app = test_app(data.path());

    let (status, headers, body) = send(&app, get("/api/does-not-exist")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    assert!(body.is_empty());

    let (status, _, body) = send(&app, get("/api")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty());

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/unknown")
        .body(Body::empty())
        .expect("request");
    let (status, _, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty());
}

#[tokio::test]
async fn blocks_path_traversal() {
    let data = TempDir::new().expect("tempdir");
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
    let data = TempDir::new().expect("tempdir");
    let assets = Arc::new(WebAssets::new(WebConfig {
        data_dir: data.path().to_path_buf(),
        theme: "missing-theme".to_owned(),
        ..WebConfig::default()
    }));
    let app = router(assets.clone());

    assert_eq!(assets.effective_theme(), "default");

    let (status, _, body) = send(&app, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains(r#"id="root""#));
}

#[tokio::test]
async fn custom_theme_overrides_default_and_falls_back_per_file() {
    let data = TempDir::new().expect("tempdir");
    write_theme(
        data.path(),
        "acme",
        "<html><head><title>Komari Monitor</title></head><body>ACME</body></html>",
    );

    let assets = Arc::new(WebAssets::new(WebConfig {
        data_dir: data.path().to_path_buf(),
        theme: "acme".to_owned(),
        ..WebConfig::default()
    }));
    let app = router(assets);

    let (status, _, body) = send(&app, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("ACME"));

    let (status, headers, body) = send(&app, get("/themes/acme/dist/index.html")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert!(String::from_utf8_lossy(&body).contains("ACME"));

    // A file that only exists in the embedded default theme is still served.
    let (status, _, body) = send(&app, get("/assets/index-CVMackoC.js")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("placeholder"));
}

#[tokio::test]
async fn lists_themes_and_skips_invalid_configs() {
    let data = TempDir::new().expect("tempdir");
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
    assert_eq!(themes[0].name, "Komari");
    assert_eq!(themes[0].version, "1.0.0");
    assert_eq!(themes[0].author, "Akizon77");
    assert_eq!(themes[1].short, "acme");
    assert_eq!(themes[1].description, "test theme");
}

#[test]
fn theme_config_requires_name_and_short() {
    let valid =
        ThemeInfo::from_json(r#"{"name":"Theme","short":"theme","version":"1.0.0","author":"me"}"#)
            .expect("valid theme");
    assert_eq!(valid.short, "theme");
    assert_eq!(valid.configuration, serde_json::Value::Null);

    let missing_name = ThemeInfo::from_json(r#"{"short":"theme"}"#).expect_err("missing name");
    assert!(missing_name.to_string().contains("name"));

    let missing_short = ThemeInfo::from_json(r#"{"name":"Theme"}"#).expect_err("missing short");
    assert!(missing_short.to_string().contains("short"));

    let invalid = ThemeInfo::from_json("{").expect_err("invalid json");
    assert!(invalid.to_string().contains("invalid theme JSON"));
}

#[tokio::test]
async fn preview_images_support_both_spellings() {
    let data = TempDir::new().expect("tempdir");
    let app = test_app(data.path());

    for path in ["/themes/default/preview.png", "/themes/default/perview.png"] {
        let (status, headers, body) = send(&app, get(path)).await;
        assert_eq!(status, StatusCode::OK, "path {path}");
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("image/png"),
            "path {path}"
        );
        assert!(!body.is_empty());
    }

    // A theme that only ships the historical `perview.png` spelling still gets
    // a preview through the fallback list.
    let theme = write_theme(data.path(), "legacy", "<html></html>");
    fs::remove_file(theme.join("preview.png")).expect("remove preview");
    fs::write(theme.join("perview.png"), b"\x89PNG\r\n\x1a\nlegacy").expect("perview");
    fs::write(
        theme.join("komari-theme.json"),
        r#"{"name":"Legacy","short":"legacy","preview":""}"#,
    )
    .expect("legacy config");

    let assets = test_assets(data.path());
    let preview = assets.theme_preview("legacy").expect("legacy preview");
    assert_eq!(preview.content_type, "image/png");
}

#[tokio::test]
async fn etag_revalidation_returns_304() {
    let data = TempDir::new().expect("tempdir");
    let app = test_app(data.path());

    let (status, headers, body) = send(&app, get("/assets/index-CVMackoC.css")).await;
    assert_eq!(status, StatusCode::OK);
    let etag = headers
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .expect("etag")
        .to_owned();
    assert!(!body.is_empty());

    let request = Request::builder()
        .uri("/assets/index-CVMackoC.css")
        .header(header::IF_NONE_MATCH, &etag)
        .body(Body::empty())
        .expect("request");
    let (status, headers, body) = send(&app, request).await;

    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert_eq!(
        headers.get(header::ETAG).and_then(|v| v.to_str().ok()),
        Some(etag.as_str())
    );
    assert!(headers.contains_key(header::CACHE_CONTROL));
    assert!(body.is_empty());
}

#[tokio::test]
async fn head_requests_return_headers_without_a_body() {
    let data = TempDir::new().expect("tempdir");
    let app = test_app(data.path());

    let request = Request::builder()
        .method(Method::HEAD)
        .uri("/assets/index-CVMackoC.js")
        .body(Body::empty())
        .expect("request");
    let (status, headers, body) = send(&app, request).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    assert!(
        headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
            > 0
    );
}

#[tokio::test]
async fn index_replacements_apply_and_admin_is_untouched() {
    let data = TempDir::new().expect("tempdir");
    let assets = test_assets(data.path());
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

    let (status, _, body) = send(&app, get("/admin/dashboard")).await;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("<title>Komari Monitor</title>"));
    assert!(!html.contains("My Site"));
    assert!(!html.contains("custom"));
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
    let data = TempDir::new().expect("tempdir");
    let app = test_app(data.path());

    let (status, headers, body) = send(&app, get("/favicon.ico")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("image/x-icon")
    );
    assert!(!body.is_empty());

    fs::write(data.path().join("favicon.ico"), b"custom-favicon").expect("favicon");
    let (status, _, body) = send(&app, get("/favicon.ico")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), b"custom-favicon");
}

#[tokio::test]
async fn precompressed_variants_are_optional() {
    let data = TempDir::new().expect("tempdir");
    let theme = write_theme(data.path(), "acme", "<html></html>");
    fs::write(theme.join("dist/assets/app-abc12345.js"), b"identity").expect("identity");
    fs::write(theme.join("dist/assets/app-abc12345.js.gz"), b"gzip-bytes").expect("gzip");
    fs::write(
        theme.join("dist/assets/app-abc12345.js.br"),
        b"brotli-bytes",
    )
    .expect("brotli");

    let assets = Arc::new(WebAssets::new(WebConfig {
        data_dir: data.path().to_path_buf(),
        theme: "acme".to_owned(),
        ..WebConfig::default()
    }));
    let app = router(assets);

    let (status, headers, body) =
        send(&app, get_with_encoding("/assets/app-abc12345.js", "gzip")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("gzip")
    );
    assert_eq!(
        headers.get(header::VARY).and_then(|v| v.to_str().ok()),
        Some("Accept-Encoding")
    );
    assert_eq!(body.as_ref(), b"gzip-bytes");

    let (status, headers, body) =
        send(&app, get_with_encoding("/assets/app-abc12345.js", "br")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("br")
    );
    assert_eq!(body.as_ref(), b"brotli-bytes");

    let (status, headers, body) = send(&app, get("/assets/app-abc12345.js")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::CONTENT_ENCODING).is_none());
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

#[test]
fn theme_layout_reports_missing_parts() {
    let missing = TempDir::new().expect("tempdir");
    assert!(matches!(
        validate_theme_layout(missing.path().join("nope")),
        Err(ThemeLayoutError::MissingDirectory(_))
    ));

    let no_dist = TempDir::new().expect("tempdir");
    assert!(matches!(
        validate_theme_layout(no_dist.path()),
        Err(ThemeLayoutError::MissingDist(_))
    ));

    let no_index = TempDir::new().expect("tempdir");
    fs::create_dir(no_index.path().join("dist")).expect("dist");
    fs::write(no_index.path().join("komari-theme.json"), "{}").expect("config");
    fs::write(no_index.path().join("preview.png"), b"png").expect("preview");
    assert!(matches!(
        validate_theme_layout(no_index.path()),
        Err(ThemeLayoutError::MissingIndex(_))
    ));

    let no_preview = TempDir::new().expect("tempdir");
    fs::create_dir(no_preview.path().join("dist")).expect("dist");
    fs::write(no_preview.path().join("dist/index.html"), "html").expect("index");
    fs::write(no_preview.path().join("komari-theme.json"), "{}").expect("config");
    assert!(matches!(
        validate_theme_layout(no_preview.path()),
        Err(ThemeLayoutError::MissingPreview(_))
    ));

    let complete = TempDir::new().expect("tempdir");
    fs::create_dir(complete.path().join("dist")).expect("dist");
    fs::write(complete.path().join("dist/index.html"), "html").expect("index");
    fs::write(complete.path().join("komari-theme.json"), "{}").expect("config");
    fs::write(complete.path().join("perview.png"), b"png").expect("preview");
    let layout = validate_theme_layout(complete.path()).expect("valid layout");
    assert!(layout.preview.ends_with("perview.png"));
}

#[test]
fn routes_compose_with_an_application_state() {
    #[derive(Clone)]
    struct AppState;

    let data = TempDir::new().expect("tempdir");
    let assets = test_assets(data.path());

    let _: Router<AppState> = routes_for::<AppState>(assets.clone());
    let _: Router<AppState> = router_for::<AppState>(assets);
}

#[test]
fn content_types_cover_theme_assets() {
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
    assert_eq!(
        content_type_for("dist/manifest.json"),
        "application/json; charset=utf-8"
    );
    assert_eq!(content_type_for("dist/favicon.ico"), "image/x-icon");
    assert_eq!(content_type_for("dist/assets/font.woff2"), "font/woff2");
    assert_eq!(content_type_for("dist/assets/logo.svg"), "image/svg+xml");
    assert_eq!(
        content_type_for("dist/assets/unknown.bin"),
        "application/octet-stream"
    );
}
