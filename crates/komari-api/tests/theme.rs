//! Theme upload / update / import endpoints.
//!
//! Success paths for remote themes use an injected [`ThemeFetcher`]; SSRF and
//! scheme checks exercise the real [`HttpThemeFetcher`] (they fail before any
//! network connection).

mod common;

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::{admin_app, call, create_client, default_config, memory_db, request, set_api_key};
use komari_api::{ApiState, ThemeFetchError, ThemeFetcher};
use serde_json::{Value, json};

const API_KEY: &str = "0123456789abcdef";
const REMOTE_URL: &str = "https://themes.example/alpha.zip";

fn theme_app(data_dir: &Path, fetcher: Arc<dyn ThemeFetcher>) -> (ApiState, Router) {
    let state = ApiState::with_config(memory_db(), default_config())
        .with_web_assets(Arc::new(komari_web::WebAssets::with_data_dir(data_dir)))
        .with_theme_fetcher(fetcher);
    let router = komari_api::admin::admin_router_with_state(state.clone());
    (state, router)
}

fn authenticated(method: Method, uri: &str, body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {API_KEY}"))
        .body(body.into())
        .expect("build request")
}

fn theme_zip(short: &str, version: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("komari-theme.json", options).unwrap();
        let config = json!({
            "name": format!("Theme {short}"),
            "short": short,
            "description": "integration test theme",
            "version": version,
            "author": "tests",
            "preview": "preview.png",
        })
        .to_string();
        zip.write_all(config.as_bytes()).unwrap();
        zip.start_file("dist/index.html", options).unwrap();
        zip.write_all(b"<html>theme</html>").unwrap();
        zip.start_file("preview.png", options).unwrap();
        zip.write_all(b"\x89PNG\r\n\x1a\n").unwrap();
        for (name, data) in extra {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }
    buffer.into_inner()
}

fn raw_zip_config(config: &str, entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("komari-theme.json", options).unwrap();
        zip.write_all(config.as_bytes()).unwrap();
        for (name, data) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }
    buffer.into_inner()
}

fn zip_bomb() -> Vec<u8> {
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("komari-theme.json", options).unwrap();
        zip.write_all(br#"{"name":"Bomb","short":"bomb"}"#).unwrap();
        zip.start_file("bomb.bin", options).unwrap();
        let chunk = vec![0u8; 1024 * 1024];
        // 129 MiB, one MiB over the extraction limit.
        for _ in 0..129 {
            zip.write_all(&chunk).unwrap();
        }
        zip.finish().unwrap();
    }
    buffer.into_inner()
}

fn multipart_upload(archive: &[u8], filename: &str) -> Request<Body> {
    let boundary = "----komari-theme-test";
    let mut payload = Vec::new();
    payload.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/zip\r\n\r\n"
        )
        .as_bytes(),
    );
    payload.extend_from_slice(archive);
    payload.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    Request::builder()
        .method(Method::PUT)
        .uri("/api/admin/theme/upload")
        .header(header::AUTHORIZATION, format!("Bearer {API_KEY}"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(payload))
        .expect("build multipart request")
}

fn raw_upload(archive: &[u8]) -> Request<Body> {
    Request::builder()
        .method(Method::PUT)
        .uri("/api/admin/theme/upload")
        .header(header::AUTHORIZATION, format!("Bearer {API_KEY}"))
        .header(header::CONTENT_TYPE, "application/zip")
        .body(Body::from(archive.to_vec()))
        .expect("build raw request")
}

#[derive(Default)]
struct FakeFetcher {
    downloads: Mutex<HashMap<String, Result<Vec<u8>, String>>>,
    releases: Mutex<HashMap<String, Result<String, String>>>,
    download_calls: Mutex<Vec<String>>,
    release_calls: Mutex<Vec<String>>,
}

impl FakeFetcher {
    fn downloaded(&self) -> Vec<String> {
        self.download_calls.lock().unwrap().clone()
    }

    fn released(&self) -> Vec<String> {
        self.release_calls.lock().unwrap().clone()
    }

    fn download(mut self, url: &str, data: Vec<u8>) -> Self {
        self.downloads
            .get_mut()
            .unwrap()
            .insert(url.to_owned(), Ok(data));
        self
    }

    fn download_error(mut self, url: &str, error: &str) -> Self {
        self.downloads
            .get_mut()
            .unwrap()
            .insert(url.to_owned(), Err(error.to_owned()));
        self
    }

    fn release(mut self, owner: &str, repo: &str, url: &str) -> Self {
        self.releases
            .get_mut()
            .unwrap()
            .insert(format!("{owner}/{repo}"), Ok(url.to_owned()));
        self
    }

    fn release_error(mut self, owner: &str, repo: &str, error: &str) -> Self {
        self.releases
            .get_mut()
            .unwrap()
            .insert(format!("{owner}/{repo}"), Err(error.to_owned()));
        self
    }
}

#[async_trait]
impl ThemeFetcher for FakeFetcher {
    async fn download(&self, url: &str) -> Result<Vec<u8>, ThemeFetchError> {
        self.download_calls.lock().unwrap().push(url.to_owned());
        match self.downloads.lock().unwrap().get(url) {
            Some(Ok(data)) => Ok(data.clone()),
            Some(Err(error)) => Err(ThemeFetchError::new(error.clone())),
            None => Err(ThemeFetchError::new(format!(
                "下载主题文件失败: unexpected URL {url}"
            ))),
        }
    }

    async fn github_release(&self, owner: &str, repo: &str) -> Result<String, ThemeFetchError> {
        let key = format!("{owner}/{repo}");
        self.release_calls.lock().unwrap().push(key.clone());
        match self.releases.lock().unwrap().get(&key) {
            Some(Ok(url)) => Ok(url.clone()),
            Some(Err(error)) => Err(ThemeFetchError::new(error.clone())),
            None => Err(ThemeFetchError::new(
                "获取GitHub release信息失败: unexpected repository",
            )),
        }
    }
}

fn write_theme(directory: &Path, short: &str, config: Value) {
    let root = directory.join("theme").join(short);
    std::fs::create_dir_all(root.join("dist")).unwrap();
    std::fs::write(root.join("komari-theme.json"), config.to_string()).unwrap();
    std::fs::write(root.join("dist/index.html"), "<html>old</html>").unwrap();
}

#[tokio::test]
async fn theme_upload_requires_admin() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());

    let (status, _) = call(
        &app,
        request(Method::PUT, "/api/admin/theme/upload", Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let client = create_client(&state, "agent");
    let (status, _) = call(
        &app,
        request(
            Method::PUT,
            &format!("/api/admin/theme/upload?token={}", client.token),
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn theme_upload_multipart_installs_theme() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    let archive = theme_zip("alpha", "1.0.0", &[("assets/app.js", b"console.log(1)")]);
    let (status, body) = call(&app, multipart_upload(&archive, "alpha.zip")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "success");
    assert_eq!(body["message"], "主题上传成功");
    assert_eq!(body["data"]["short"], "alpha");
    assert_eq!(body["data"]["version"], "1.0.0");

    let root = directory.path().join("theme/alpha");
    assert!(root.join("komari-theme.json").is_file());
    assert!(root.join("dist/index.html").is_file());
    assert!(root.join("preview.png").is_file());
    assert!(root.join("assets/app.js").is_file());
}

#[tokio::test]
async fn theme_upload_accepts_raw_body() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    let archive = theme_zip("raw", "1.0.0", &[]);
    let (status, body) = call(&app, raw_upload(&archive)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["short"], "raw");
    assert!(directory.path().join("theme/raw/dist/index.html").is_file());
}

#[tokio::test]
async fn theme_upload_rejects_invalid_zip() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    let (status, body) = call(&app, multipart_upload(b"not a zip", "bad.zip")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("无法打开ZIP文件"),
        "{body}"
    );
}

#[tokio::test]
async fn theme_upload_rejects_missing_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    let mut buffer = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("dist/index.html", options).unwrap();
        zip.write_all(b"<html></html>").unwrap();
        zip.finish().unwrap();
    }
    let (status, body) = call(&app, multipart_upload(&buffer.into_inner(), "x.zip")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("komari-theme.json 不存在"),
        "{body}"
    );
}

#[tokio::test]
async fn theme_upload_rejects_missing_required_fields_and_unsafe_short() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    let archive = raw_zip_config(r#"{"name":"No short"}"#, &[]);
    let (status, body) = call(&app, multipart_upload(&archive, "x.zip")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"].as_str().unwrap().contains("缺少必填字段"),
        "{body}"
    );

    let archive = raw_zip_config(r#"{"name":"Evil","short":"../evil"}"#, &[]);
    let (status, body) = call(&app, multipart_upload(&archive, "x.zip")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("short字段格式无效"),
        "{body}"
    );
    assert!(!directory.path().join("evil").exists());
}

#[tokio::test]
async fn theme_upload_returns_go_shaped_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    // Missing configuration -> Go's zero-value object.
    let archive = raw_zip_config(r#"{"name":"No Config","short":"noconfig"}"#, &[]);
    let (status, body) = call(&app, multipart_upload(&archive, "noconfig.zip")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["configuration"],
        json!({"type": "", "icon": "", "name": null, "data": null})
    );

    // Partial configuration keeps the remaining zero values.
    let archive = raw_zip_config(
        r#"{"name":"Partial","short":"partial","configuration":{"type":"managed","data":[1]}}"#,
        &[],
    );
    let (status, body) = call(&app, multipart_upload(&archive, "partial.zip")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["configuration"],
        json!({"type": "managed", "icon": "", "name": null, "data": [1]})
    );

    // Non-object configuration is a JSON error, like Go's struct decoder.
    for configuration in [r#""managed""#, "[]", "1", "true"] {
        let config = format!(r#"{{"name":"Bad","short":"bad","configuration":{configuration}}}"#);
        let archive = raw_zip_config(&config, &[]);
        let (status, body) = call(&app, multipart_upload(&archive, "bad.zip")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{configuration}: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("主题配置格式错误"),
            "{configuration}: {body}"
        );
        assert!(!directory.path().join("theme/bad").exists());
    }
}

#[tokio::test]
async fn theme_upload_accepts_whitespace_name_and_null_strings() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    // Go only checks `name == ""`, so a whitespace-only name is accepted; JSON
    // nulls in string fields decode to the zero value.
    let archive = raw_zip_config(
        r#"{"name":" ","short":"blank","description":null,"version":null,"author":null,"url":null,"preview":null,"configuration":null}"#,
        &[],
    );
    let (status, body) = call(&app, multipart_upload(&archive, "blank.zip")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["name"], " ");
    assert_eq!(body["data"]["description"], "");
    assert_eq!(body["data"]["version"], "");
    assert_eq!(
        body["data"]["configuration"],
        json!({"type": "", "icon": "", "name": null, "data": null})
    );
}

#[tokio::test]
async fn theme_upload_skips_zip_slip_entries() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    let archive = raw_zip_config(
        r#"{"name":"Slip","short":"slip"}"#,
        &[
            ("../escaped.txt", b"outside"),
            ("dist/../../escaped2.txt", b"outside"),
            ("dist/index.html", b"<html></html>"),
        ],
    );
    let (status, body) = call(&app, multipart_upload(&archive, "slip.zip")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        directory
            .path()
            .join("theme/slip/dist/index.html")
            .is_file()
    );
    assert!(!directory.path().join("escaped.txt").exists());
    assert!(!directory.path().join("theme/escaped.txt").exists());
    assert!(!directory.path().join("theme/slip/escaped.txt").exists());
}

#[tokio::test]
async fn theme_upload_rejects_oversized_archive() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    // 65 MiB exceeds the 64 MiB handler cap.
    let payload = vec![0u8; 65 * 1024 * 1024];
    let (status, body) = call(&app, multipart_upload(&payload, "large.zip")).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert!(body["message"].as_str().unwrap().contains("过大"), "{body}");
}

#[tokio::test]
async fn theme_upload_rejects_zip_bomb() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    let archive = zip_bomb();
    let (status, body) = call(&app, multipart_upload(&archive, "bomb.zip")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("解压后过大"),
        "{body}"
    );
    assert!(!directory.path().join("theme/bomb").exists());
}

#[tokio::test]
async fn theme_upload_overwrites_same_short() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    let root = directory.path().join("theme/alpha");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("old.txt"), "old").unwrap();

    let archive = theme_zip("alpha", "2.0.0", &[]);
    let (status, body) = call(&app, multipart_upload(&archive, "alpha.zip")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["version"], "2.0.0");
    assert!(!root.join("old.txt").exists());
    assert!(root.join("dist/index.html").is_file());
}

#[tokio::test]
async fn theme_upload_failure_preserves_existing_theme() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    let root = directory.path().join("theme/alpha");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("marker.txt"), "keep me").unwrap();

    // A file entry `conflict` followed by `conflict/nested` cannot be
    // extracted: the parent is already a regular file.
    let archive = raw_zip_config(
        r#"{"name":"Alpha","short":"alpha"}"#,
        &[("conflict", b"file"), ("conflict/nested", b"dir")],
    );
    let (status, body) = call(&app, multipart_upload(&archive, "alpha.zip")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        std::fs::read_to_string(root.join("marker.txt")).unwrap(),
        "keep me"
    );
    assert!(!root.join("dist/index.html").exists());
    assert!(!directory.path().join(".theme-staging").exists());
}

#[tokio::test]
async fn theme_import_preview_returns_metadata_without_installing() {
    let directory = tempfile::tempdir().unwrap();
    let fetcher = FakeFetcher::default().download(REMOTE_URL, theme_zip("alpha", "1.0.0", &[]));
    let (state, app) = theme_app(directory.path(), Arc::new(fetcher));
    set_api_key(&state, API_KEY);

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/import?preview=true",
            json!({"url": REMOTE_URL}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["theme"]["short"], "alpha");
    assert_eq!(body["data"]["exists"], false);
    assert!(!directory.path().join("theme/alpha").exists());
}

#[tokio::test]
async fn theme_import_installs_and_reports_overwrite() {
    let directory = tempfile::tempdir().unwrap();
    let fetcher = FakeFetcher::default().download(REMOTE_URL, theme_zip("alpha", "1.0.0", &[]));
    let (state, app) = theme_app(directory.path(), Arc::new(fetcher));
    set_api_key(&state, API_KEY);

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/import",
            json!({"url": REMOTE_URL}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["message"], "主题导入成功");
    assert!(
        directory
            .path()
            .join("theme/alpha/dist/index.html")
            .is_file()
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/import?preview=true",
            json!({"url": REMOTE_URL}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["exists"], true);

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/import",
            json!({"url": REMOTE_URL}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["message"], "主题导入成功（已覆盖同名主题）");
}

#[tokio::test]
async fn theme_import_blocks_private_and_non_http_urls() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());
    set_api_key(&state, API_KEY);

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/import",
            json!({"url": "http://127.0.0.1:9/theme.zip"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("private/internal addresses"),
        "{body}"
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/import",
            json!({"url": "http://192.168.1.10/theme.zip"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("private/internal addresses"),
        "{body}"
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/import",
            json!({"url": "file:///tmp/theme.zip"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("only http and https"),
        "{body}"
    );
}

#[tokio::test]
async fn theme_import_reports_http_status_and_timeout() {
    let directory = tempfile::tempdir().unwrap();
    let fetcher = FakeFetcher::default()
        .download_error(REMOTE_URL, "下载主题文件失败，HTTP状态码: 404")
        .download_error(
            "https://themes.example/slow.zip",
            "下载主题文件失败: operation timed out",
        );
    let (state, app) = theme_app(directory.path(), Arc::new(fetcher));
    set_api_key(&state, API_KEY);

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/import",
            json!({"url": REMOTE_URL}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["message"],
        "下载主题失败: 下载主题文件失败，HTTP状态码: 404"
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/import",
            json!({"url": "https://themes.example/slow.zip"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("timed out"),
        "{body}"
    );
}

#[tokio::test]
async fn theme_import_uses_github_latest_release() {
    let directory = tempfile::tempdir().unwrap();
    let fetcher = FakeFetcher::default()
        .release("owner", "repo", REMOTE_URL)
        .download(REMOTE_URL, theme_zip("github", "1.2.3", &[]));
    let (state, app) = theme_app(directory.path(), Arc::new(fetcher));
    set_api_key(&state, API_KEY);

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/import",
            json!({"url": "https://github.com/owner/repo"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["short"], "github");
    assert_eq!(body["data"]["version"], "1.2.3");
}

#[tokio::test]
async fn theme_update_requires_existing_theme() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/update",
            json!({"short": "missing"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["message"], "主题不存在");
}

#[tokio::test]
async fn theme_update_without_download_source_fails() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);
    write_theme(
        directory.path(),
        "alpha",
        json!({"name": "Alpha", "short": "alpha", "version": "1.0.0"}),
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/update",
            json!({"short": "alpha"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["message"],
        "无法下载主题，请提供有效的URL或GitHub仓库信息"
    );
}

#[tokio::test]
async fn theme_update_downloads_from_original_url() {
    let directory = tempfile::tempdir().unwrap();
    let fetcher = FakeFetcher::default().download(REMOTE_URL, theme_zip("alpha", "2.0.0", &[]));
    let (state, app) = theme_app(directory.path(), Arc::new(fetcher));
    set_api_key(&state, API_KEY);
    write_theme(
        directory.path(),
        "alpha",
        json!({"name": "Alpha", "short": "alpha", "version": "1.0.0", "url": REMOTE_URL}),
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/update",
            json!({"short": "alpha"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["message"], "主题更新成功");
    assert_eq!(body["data"]["version"], "2.0.0");
    assert_eq!(
        std::fs::read_to_string(directory.path().join("theme/alpha/dist/index.html")).unwrap(),
        "<html>theme</html>"
    );
}

#[tokio::test]
async fn theme_update_resolves_github_original_url_to_latest_release() {
    let directory = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(
        FakeFetcher::default()
            .release("owner", "repo", REMOTE_URL)
            .download(REMOTE_URL, theme_zip("alpha", "4.0.0", &[])),
    );
    let (state, app) = theme_app(directory.path(), fetcher.clone());
    set_api_key(&state, API_KEY);
    write_theme(
        directory.path(),
        "alpha",
        json!({"name": "Alpha", "short": "alpha", "version": "1.0.0", "url": "https://github.com/owner/repo"}),
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/update",
            json!({"short": "alpha"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["version"], "4.0.0");
    assert_eq!(fetcher.released(), vec!["owner/repo"]);
    assert_eq!(fetcher.downloaded(), vec![REMOTE_URL]);
}

#[tokio::test]
async fn theme_update_resolves_explicit_github_url_to_latest_release() {
    let directory = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(
        FakeFetcher::default()
            .release("owner", "repo", REMOTE_URL)
            .download(REMOTE_URL, theme_zip("alpha", "4.1.0", &[])),
    );
    let (state, app) = theme_app(directory.path(), fetcher.clone());
    set_api_key(&state, API_KEY);
    write_theme(
        directory.path(),
        "alpha",
        json!({"name": "Alpha", "short": "alpha", "version": "1.0.0"}),
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/update",
            json!({"short": "alpha", "url": "https://github.com/owner/repo.git"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["version"], "4.1.0");
    assert_eq!(fetcher.released(), vec!["owner/repo"]);
    assert_eq!(fetcher.downloaded(), vec![REMOTE_URL]);
}

#[tokio::test]
async fn theme_update_falls_back_to_git_owner_after_original_url_failure() {
    let directory = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(
        FakeFetcher::default()
            .download_error(
                "https://themes.example/broken.zip",
                "下载主题文件失败，HTTP状态码: 500",
            )
            .release("owner", "repo", REMOTE_URL)
            .download(REMOTE_URL, theme_zip("alpha", "5.0.0", &[])),
    );
    let (state, app) = theme_app(directory.path(), fetcher.clone());
    set_api_key(&state, API_KEY);
    write_theme(
        directory.path(),
        "alpha",
        json!({"name": "Alpha", "short": "alpha", "version": "1.0.0", "url": "https://themes.example/broken.zip"}),
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/update",
            json!({"short": "alpha", "git_owner": "owner", "git_repo": "repo"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["version"], "5.0.0");
    assert_eq!(fetcher.released(), vec!["owner/repo"]);
    assert_eq!(
        fetcher.downloaded(),
        vec!["https://themes.example/broken.zip", REMOTE_URL]
    );
}

#[tokio::test]
async fn theme_update_falls_back_to_new_url_after_original_url_failure() {
    let directory = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(
        FakeFetcher::default()
            .download_error(
                "https://themes.example/broken.zip",
                "下载主题文件失败，HTTP状态码: 500",
            )
            .download(REMOTE_URL, theme_zip("alpha", "5.1.0", &[])),
    );
    let (state, app) = theme_app(directory.path(), fetcher.clone());
    set_api_key(&state, API_KEY);
    write_theme(
        directory.path(),
        "alpha",
        json!({"name": "Alpha", "short": "alpha", "version": "1.0.0", "url": "https://themes.example/broken.zip"}),
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/update",
            json!({"short": "alpha", "url": REMOTE_URL}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["version"], "5.1.0");
    assert!(fetcher.released().is_empty());
    assert_eq!(
        fetcher.downloaded(),
        vec!["https://themes.example/broken.zip", REMOTE_URL]
    );
}

#[tokio::test]
async fn theme_update_uses_explicit_github_repository() {
    let directory = tempfile::tempdir().unwrap();
    let fetcher = FakeFetcher::default()
        .release("owner", "repo", REMOTE_URL)
        .download(REMOTE_URL, theme_zip("alpha", "3.0.0", &[]));
    let (state, app) = theme_app(directory.path(), Arc::new(fetcher));
    set_api_key(&state, API_KEY);
    write_theme(
        directory.path(),
        "alpha",
        json!({"name": "Alpha", "short": "alpha", "version": "1.0.0"}),
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/update",
            json!({"short": "alpha", "git_owner": "owner", "git_repo": "repo"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["version"], "3.0.0");
}

#[tokio::test]
async fn theme_update_reports_download_failure() {
    let directory = tempfile::tempdir().unwrap();
    let fetcher = FakeFetcher::default().download_error(
        "https://themes.example/broken.zip",
        "下载主题文件失败，HTTP状态码: 500",
    );
    let (state, app) = theme_app(directory.path(), Arc::new(fetcher));
    set_api_key(&state, API_KEY);
    write_theme(
        directory.path(),
        "alpha",
        json!({"name": "Alpha", "short": "alpha", "version": "1.0.0"}),
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/update",
            json!({"short": "alpha", "url": "https://themes.example/broken.zip"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["message"],
        "从新URL下载主题失败: 下载主题文件失败，HTTP状态码: 500"
    );
}

#[tokio::test]
async fn theme_update_reports_github_release_failure() {
    let directory = tempfile::tempdir().unwrap();
    let fetcher = FakeFetcher::default().release_error(
        "owner",
        "repo",
        "获取GitHub release信息失败，HTTP状态码: 404",
    );
    let (state, app) = theme_app(directory.path(), Arc::new(fetcher));
    set_api_key(&state, API_KEY);
    write_theme(
        directory.path(),
        "alpha",
        json!({"name": "Alpha", "short": "alpha", "version": "1.0.0"}),
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/update",
            json!({"short": "alpha", "git_owner": "owner", "git_repo": "repo"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["message"],
        "从GitHub获取下载链接失败: 获取GitHub release信息失败，HTTP状态码: 404"
    );
}

#[tokio::test]
async fn theme_update_rejects_invalid_downloaded_zip() {
    let directory = tempfile::tempdir().unwrap();
    let fetcher = FakeFetcher::default().download(REMOTE_URL, b"not a zip".to_vec());
    let (state, app) = theme_app(directory.path(), Arc::new(fetcher));
    set_api_key(&state, API_KEY);
    write_theme(
        directory.path(),
        "alpha",
        json!({"name": "Alpha", "short": "alpha", "version": "1.0.0", "url": REMOTE_URL}),
    );

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/update",
            json!({"short": "alpha"}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("无法打开ZIP文件"),
        "{body}"
    );
}

#[tokio::test]
async fn theme_import_requires_url() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = theme_app(directory.path(), Arc::new(FakeFetcher::default()));
    set_api_key(&state, API_KEY);

    let (status, body) = call(
        &app,
        authenticated(
            Method::POST,
            "/api/admin/theme/import",
            json!({}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "参数错误: missing url");
}
