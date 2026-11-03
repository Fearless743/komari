mod common;

use std::io::{Cursor, Read, Write};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::{admin_app, call, create_admin_session, create_client, request, set_api_key};
use komari_api::ApiState;
use komari_db::models::Provider;
use serde_json::{Value, json};
use tower::ServiceExt;

const API_KEY: &str = "0123456789abcdef";

fn bearer(method: Method, uri: &str, body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {API_KEY}"))
        .body(body.into())
        .expect("build request")
}

fn with_cookie(mut request: Request<Body>, cookie: &str) -> Request<Body> {
    request
        .headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    request
}

fn with_cookies(mut request: Request<Body>, cookies: &[&str]) -> Request<Body> {
    request
        .headers_mut()
        .insert(header::COOKIE, cookies.join("; ").parse().unwrap());
    request
}

fn with_header(mut request: Request<Body>, name: header::HeaderName, value: &str) -> Request<Body> {
    request.headers_mut().insert(name, value.parse().unwrap());
    request
}

fn admin_key(state: &ApiState) {
    set_api_key(state, API_KEY);
}

fn json_body(value: Value) -> Body {
    Body::from(value.to_string())
}

#[tokio::test]
async fn admin_settings_requires_admin() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());

    let (status, body) = call(
        &app,
        request(Method::GET, "/api/admin/settings", Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["message"], "Unauthorized.");

    let client = create_client(&state, "agent");
    let (status, _) = call(
        &app,
        request(
            Method::GET,
            &format!("/api/admin/settings?token={}", client.token),
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    admin_key(&state);
    let (status, body) = call(
        &app,
        bearer(Method::GET, "/api/admin/settings", Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
    assert!(body["data"].is_object());
}

#[tokio::test]
async fn admin_settings_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());
    admin_key(&state);

    let (status, body) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/settings",
            json_body(json!({"sitename": "Example", "allow_cors": true})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
    assert!(body.get("data").is_none());

    let (_, body) = call(
        &app,
        bearer(Method::GET, "/api/admin/settings", Body::empty()),
    )
    .await;
    assert_eq!(body["data"]["sitename"], "Example");
    assert_eq!(body["data"]["allow_cors"], true);

    let (status, body) = call(
        &app,
        bearer(Method::POST, "/api/admin/settings", Body::from("[1,2]")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["status"], "error");
}

#[tokio::test]
async fn admin_settings_apply_provider_and_theme_side_effects() {
    let directory = tempfile::tempdir().unwrap();
    let theme_root = directory.path().join("theme").join("alpha");
    std::fs::create_dir_all(theme_root.join("dist")).unwrap();
    std::fs::write(
        theme_root.join("komari-theme.json"),
        r#"{"name":"Alpha","short":"alpha"}"#,
    )
    .unwrap();
    std::fs::write(theme_root.join("dist/index.html"), "<html></html>").unwrap();

    let (state, app) = admin_app(directory.path());
    admin_key(&state);
    state
        .db
        .ddns_providers()
        .save(&Provider::new(
            "cloudflare",
            r#"{"api_token":"token","zone_id":"zone","record_id":"record"}"#,
        ))
        .unwrap();

    let (status, _) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/settings",
            json_body(json!({
                "ddns_provider": "cloudflare",
                "ddns_enabled": true,
                "theme": "alpha",
                "sitename": "Side Effects"
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let provider = state
        .ddns
        .current_provider()
        .expect("DDNS provider should be loaded");
    assert_eq!(provider.name(), "cloudflare");
    assert_eq!(state.web.theme(), "alpha");
    assert_eq!(state.web.site_meta().sitename, "Side Effects");
}

#[tokio::test]
async fn admin_clients_crud_weight_and_hidden() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());
    admin_key(&state);

    let (status, body) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/client/add",
            json_body(json!({"name": "alpha"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
    let uuid = body["uuid"].as_str().unwrap().to_owned();
    let token = body["token"].as_str().unwrap().to_owned();
    assert_eq!(body["message"], "");

    let (status, body) = call(
        &app,
        bearer(Method::GET, "/api/admin/client/list", Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["name"], "alpha");

    let (status, body) = call(
        &app,
        bearer(
            Method::GET,
            &format!("/api/admin/client/{uuid}"),
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["uuid"], uuid);

    let (status, body) = call(
        &app,
        bearer(
            Method::POST,
            &format!("/api/admin/client/{uuid}/edit"),
            json_body(json!({"hidden": true, "weight": 7, "remark": "private"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"status": "success"}));

    let (_, body) = call(
        &app,
        bearer(
            Method::GET,
            &format!("/api/admin/client/{uuid}"),
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(body["hidden"], true);
    assert_eq!(body["weight"], 7);
    assert_eq!(body["remark"], "private");

    let (status, body) = call(
        &app,
        bearer(
            Method::GET,
            &format!("/api/admin/client/{uuid}/token"),
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["token"], token);

    let (status, body) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/client/order",
            json_body(json!({uuid.clone(): 9})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"status": "success", "message": ""}));
    let (_, body) = call(
        &app,
        bearer(
            Method::GET,
            &format!("/api/admin/client/{uuid}"),
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(body["weight"], 9);

    let (status, body) = call(
        &app,
        bearer(
            Method::POST,
            &format!("/api/admin/client/{uuid}/remove"),
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"status": "success"}));
    let (_, body) = call(
        &app,
        bearer(Method::GET, "/api/admin/client/list", Body::empty()),
    )
    .await;
    assert!(body.as_array().unwrap().is_empty());

    // Missing body creates a default-named client.
    let (status, body) = call(
        &app,
        bearer(Method::POST, "/api/admin/client/add", Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["uuid"].is_string());
    assert!(body.get("message").is_none());
}

#[tokio::test]
async fn admin_ddns_get_redacts_sensitive_fields() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());
    admin_key(&state);
    state
        .db
        .ddns_providers()
        .save(&Provider::new(
            "cloudflare",
            r#"{"api_token":"super-secret","zone_id":"zone-1","record_id":"record-1"}"#,
        ))
        .unwrap();

    let (status, body) = call(
        &app,
        bearer(
            Method::GET,
            "/api/admin/settings/ddns?provider=cloudflare",
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let addition = body["data"]["addition"].as_str().unwrap();
    let addition: Value = serde_json::from_str(addition).unwrap();
    assert!(addition.get("api_token").is_none());
    // Cloudflare declares both the token and zone id as sensitive.
    assert!(addition.get("zone_id").is_none());
    assert_eq!(addition["record_id"], "record-1");

    // The database keeps the complete configuration.
    let stored = state
        .db
        .ddns_providers()
        .get("cloudflare")
        .unwrap()
        .unwrap();
    assert!(stored.addition.contains("super-secret"));

    let (status, body) = call(
        &app,
        bearer(Method::GET, "/api/admin/settings/ddns", Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["cloudflare"].is_array());

    // Invalid history query values fall back to the defaults, not a 400.
    let (status, body) = call(
        &app,
        bearer(
            Method::GET,
            "/api/admin/settings/ddns/history?limit=abc&before_days=abc",
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"].is_array());
    let (status, _) = call(
        &app,
        bearer(
            Method::DELETE,
            "/api/admin/settings/ddns/history?before_days=abc",
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn admin_notification_and_oauth_get_redact_secrets() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());
    admin_key(&state);

    state
        .db
        .message_senders()
        .save(&Provider::new(
            "telegram",
            r#"{"bot_token":"bot-secret","chat_id":"42"}"#,
        ))
        .unwrap();
    let (status, body) = call(
        &app,
        bearer(
            Method::GET,
            "/api/admin/settings/message-sender?provider=telegram",
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let addition: Value = serde_json::from_str(body["data"]["addition"].as_str().unwrap()).unwrap();
    assert!(addition.get("bot_token").is_none());
    assert_eq!(addition["chat_id"], "42");
    let stored = state.db.message_senders().get("telegram").unwrap().unwrap();
    assert!(stored.addition.contains("bot-secret"));

    state
        .db
        .oauth()
        .save(&Provider::new(
            "github",
            r#"{"client_id":"id","client_secret":"oauth-secret","app_secret":"app-secret","id_token":"jwt","code":"auth-code","state":"csrf"}"#,
        ))
        .unwrap();
    let (status, body) = call(
        &app,
        bearer(
            Method::GET,
            "/api/admin/settings/oidc?provider=github",
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let addition: Value = serde_json::from_str(body["data"]["addition"].as_str().unwrap()).unwrap();
    assert!(addition.get("client_secret").is_none());
    assert!(addition.get("app_secret").is_none());
    assert!(addition.get("id_token").is_none());
    assert!(addition.get("code").is_none());
    assert!(addition.get("state").is_none());
    assert_eq!(addition["client_id"], "id");

    let (status, body) = call(
        &app,
        bearer(
            Method::GET,
            "/api/admin/settings/message-sender",
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["telegram"].is_array());
}

#[tokio::test]
async fn admin_theme_list_set_and_delete() {
    let directory = tempfile::tempdir().unwrap();
    let theme_root = directory.path().join("theme").join("alpha");
    std::fs::create_dir_all(theme_root.join("dist")).unwrap();
    std::fs::write(
        theme_root.join("komari-theme.json"),
        r#"{"name":"Alpha","short":"alpha","description":"test theme","version":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::write(theme_root.join("dist/index.html"), "<html></html>").unwrap();

    let (state, app) = admin_app(directory.path());
    admin_key(&state);

    let (status, body) = call(
        &app,
        bearer(Method::GET, "/api/admin/theme/list", Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let themes = body["data"].as_array().unwrap();
    assert!(themes.iter().any(|theme| theme["short"] == "alpha"));

    let (status, body) = call(
        &app,
        bearer(
            Method::GET,
            "/api/admin/theme/set?theme=alpha",
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["theme"], "alpha");
    assert_eq!(
        state
            .db
            .config()
            .get_as::<String>("theme")
            .unwrap()
            .unwrap(),
        "alpha"
    );
    assert_eq!(state.web.theme(), "alpha");

    let (status, _) = call(
        &app,
        bearer(
            Method::GET,
            "/api/admin/theme/set?theme=missing",
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/theme/delete",
            json_body(json!({"short": "alpha"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!theme_root.exists());
}

#[tokio::test]
async fn admin_backup_download_contains_markup_and_data() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("extra.txt"), "hello").unwrap();
    let (state, app) = admin_app(directory.path());
    admin_key(&state);

    let response = app
        .clone()
        .oneshot(bearer(
            Method::GET,
            "/api/admin/download/backup",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/zip");
    assert!(
        response.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("backup-")
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut names = Vec::new();
    for index in 0..archive.len() {
        names.push(archive.by_index(index).unwrap().name().to_owned());
    }
    assert!(names.iter().any(|name| name == "komari-backup-markup"));
    assert!(names.iter().any(|name| name == "komari.db"));
    assert!(names.iter().any(|name| name == "extra.txt"));
    let mut markup = String::new();
    archive
        .by_name("komari-backup-markup")
        .unwrap()
        .read_to_string(&mut markup)
        .unwrap();
    assert!(markup.contains("Komari"));
}

#[tokio::test]
async fn admin_backup_upload_validates_markup_and_stages_zip() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());
    admin_key(&state);

    let mut buffer = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("komari-backup-markup", options).unwrap();
        zip.write_all(b"marker").unwrap();
        zip.finish().unwrap();
    }
    let archive = buffer.into_inner();
    let boundary = "----komari-test";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"backup\"; filename=\"backup.zip\"\r\nContent-Type: application/zip\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&archive);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/admin/upload/backup")
        .header(header::AUTHORIZATION, format!("Bearer {API_KEY}"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let (status, body) = call(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
    assert_eq!(body["path"], "./data/backup.zip");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("Backup uploaded")
    );
    assert!(body.get("data").is_none());
    assert!(directory.path().join("backup.zip").is_file());

    // A ZIP without the marker is rejected.
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buffer);
        zip.start_file("other.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();
    }
    let archive = buffer.into_inner();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"backup\"; filename=\"bad.zip\"\r\nContent-Type: application/zip\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&archive);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/admin/upload/backup")
        .header(header::AUTHORIZATION, format!("Bearer {API_KEY}"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let (status, _) = call(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_backup_upload_accepts_archives_over_two_megabytes() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());
    admin_key(&state);

    let mut buffer = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("komari-backup-markup", options).unwrap();
        zip.write_all(b"marker").unwrap();
        zip.start_file("big.bin", options).unwrap();
        zip.write_all(&vec![0x5a; 3 * 1024 * 1024]).unwrap();
        zip.finish().unwrap();
    }
    let archive = buffer.into_inner();
    assert!(archive.len() > 2 * 1024 * 1024);

    let boundary = "----komari-large";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"backup\"; filename=\"large.zip\"\r\nContent-Type: application/zip\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&archive);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/admin/upload/backup")
        .header(header::AUTHORIZATION, format!("Bearer {API_KEY}"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let (status, body) = call(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
    assert!(directory.path().join("backup.zip").is_file());
}

#[tokio::test]
async fn admin_sessions_list_and_delete() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());
    let session = create_admin_session(&state);

    let request = with_cookie(
        request(Method::GET, "/api/admin/session/get", Body::empty()),
        &format!("session_token={session}"),
    );
    let (status, body) = call(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["current"], session);
    assert_eq!(body["data"].as_array().unwrap().len(), 1);

    let (status, _) = call(
        &app,
        with_cookie(
            bearer(
                Method::POST,
                "/api/admin/session/remove",
                json_body(json!({"session": session})),
            ),
            &format!("session_token={session}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(state.db.sessions().list().unwrap().is_empty());
}

#[tokio::test]
async fn admin_two_factor_generate_enable_disable() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());
    let session = create_admin_session(&state);

    let request = with_cookie(
        request(Method::GET, "/api/admin/2fa/generate", Body::empty()),
        &format!("session_token={session}"),
    );
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_owned();
    let secret = cookie
        .split(';')
        .find_map(|part| part.trim().strip_prefix("2fa_secret="))
        .expect("2fa_secret cookie")
        .to_owned();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(body.len() > 100);

    let code = komari_auth::generate_code_at(&secret, chrono::Utc::now().timestamp()).unwrap();
    let request = with_cookies(
        bearer(
            Method::POST,
            &format!("/api/admin/2fa/enable?code={code}"),
            Body::empty(),
        ),
        &[
            &format!("session_token={session}"),
            &format!("2fa_secret={secret}"),
        ],
    );
    let (status, body) = call(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"], "2FA enabled successfully");
    let user = state
        .db
        .accounts()
        .get_by_username("admin")
        .unwrap()
        .unwrap();
    assert_eq!(user.two_factor, secret);

    let request = with_cookie(
        bearer(Method::POST, "/api/admin/2fa/disable", Body::empty()),
        &format!("session_token={session}"),
    );
    let (status, _) = call(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    let user = state
        .db
        .accounts()
        .get_by_username("admin")
        .unwrap()
        .unwrap();
    assert!(user.two_factor.is_empty());
}

#[tokio::test]
async fn admin_logs_and_clipboard() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());
    admin_key(&state);
    state
        .db
        .logs()
        .log("127.0.0.1", "user", "hello", "info")
        .unwrap();

    let (status, body) = call(&app, bearer(Method::GET, "/api/admin/logs", Body::empty())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["total"], 1);
    assert_eq!(body["data"]["logs"][0]["message"], "hello");
    assert!(body.get("total").is_none());

    let (status, _) = call(
        &app,
        bearer(Method::GET, "/api/admin/logs?limit=0", Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/clipboard",
            json_body(json!({"text": "hello", "name": "note"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = body["data"]["id"].as_i64().unwrap();

    let (status, _body) = call(
        &app,
        bearer(
            Method::POST,
            &format!("/api/admin/clipboard/{id}"),
            json_body(json!({"text": "updated"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, body) = call(
        &app,
        bearer(
            Method::GET,
            &format!("/api/admin/clipboard/{id}"),
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(body["data"]["text"], "updated");

    let (status, _) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/clipboard/remove",
            json_body(json!({"ids": [id]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(state.db.clipboard().list().unwrap().is_empty());

    let (status, _) = call(
        &app,
        bearer(
            Method::POST,
            &format!("/api/admin/clipboard/{id}/remove"),
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn admin_notification_rules() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());
    admin_key(&state);
    let client = create_client(&state, "notify-target");

    let (status, _body) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/notification/offline/edit",
            json_body(json!([{
                "client": client.uuid,
                "enable": true,
                "grace_period": 30
            }])),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = call(
        &app,
        bearer(
            Method::GET,
            "/api/admin/notification/offline",
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(body["data"][0]["client"], client.uuid);

    let (status, _) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/notification/offline/disable",
            json_body(json!([client.uuid])),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !state
            .db
            .notifications()
            .offline_get(&client.uuid)
            .unwrap()
            .unwrap()
            .enable
    );

    let (status, body) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/notification/load/add",
            json_body(json!({
                "clients": [client.uuid],
                "name": "cpu high",
                "metric": "cpu",
                "threshold": 80.0,
                "ratio": 0.8,
                "interval": 10
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let task_id = body["data"]["task_id"].as_i64().unwrap();

    let (_, body) = call(
        &app,
        bearer(Method::GET, "/api/admin/notification/load", Body::empty()),
    )
    .await;
    assert_eq!(body["data"][0]["name"], "cpu high");

    // Editing preserves fields the caller did not set, including last_notified.
    let notified_at = komari_db::LocalTime::now().add_seconds(-60);
    state
        .db
        .notifications()
        .load_set_last_notified(task_id, notified_at)
        .unwrap();
    let (status, _) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/notification/load/edit",
            json_body(json!({"notifications": [{"id": task_id, "name": "renamed"}]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = call(
        &app,
        bearer(Method::GET, "/api/admin/notification/load", Body::empty()),
    )
    .await;
    assert_eq!(body["data"][0]["name"], "renamed");
    assert_eq!(
        body["data"][0]["last_notified"],
        serde_json::to_value(notified_at).unwrap()
    );

    let (status, _) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/notification/load/delete",
            json_body(json!({"id": [task_id]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Deleting a missing rule and editing id=0 are Go 500s.
    let (status, _) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/notification/load/delete",
            json_body(json!({"id": [task_id]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let (status, _) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/notification/load/edit",
            json_body(json!({"notifications": [{"id": 0, "name": "new"}]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn admin_geoip_requires_enabled_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());
    admin_key(&state);

    let (status, body) = call(
        &app,
        bearer(
            Method::GET,
            "/api/admin/test/geoip?ip=1.1.1.1",
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["message"],
        "GeoIP is not enabled in the configuration."
    );
}

#[tokio::test]
async fn admin_user_update_and_favicon() {
    let directory = tempfile::tempdir().unwrap();
    let (state, app) = admin_app(directory.path());
    admin_key(&state);
    let session = create_admin_session(&state);
    let user = state
        .db
        .accounts()
        .get_by_username("admin")
        .unwrap()
        .unwrap();

    let (status, _) = call(
        &app,
        with_cookie(
            bearer(
                Method::POST,
                "/api/admin/update/user",
                json_body(json!({"uuid": user.uuid, "password": "new-password"})),
            ),
            &format!("session_token={session}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let updated = state
        .db
        .accounts()
        .get_by_username("admin")
        .unwrap()
        .unwrap();
    assert!(updated.check_password("new-password"));

    // The password change invalidated every session, including the caller's.
    let (status, _) = call(
        &app,
        with_cookie(
            request(Method::GET, "/api/admin/settings", Body::empty()),
            &format!("session_token={session}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A missing user is a 500, matching Go's accounts.UpdateUser error path.
    let (status, body) = call(
        &app,
        bearer(
            Method::POST,
            "/api/admin/update/user",
            json_body(json!({"uuid": "missing", "password": "new-password"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["message"], "user not found: missing");

    // 3 MB favicon is above Axum's default 2 MB limit but below the 5 MB cap.
    let request = with_header(
        bearer(
            Method::PUT,
            "/api/admin/update/favicon",
            Body::from(vec![7u8; 3 * 1024 * 1024]),
        ),
        header::CONTENT_TYPE,
        "image/x-icon",
    );
    let (status, _) = call(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    assert!(directory.path().join("favicon.ico").is_file());

    let (status, _) = call(
        &app,
        bearer(Method::POST, "/api/admin/update/favicon", Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!directory.path().join("favicon.ico").is_file());
}

#[tokio::test]
async fn admin_router_can_be_merged_without_layers() {
    let directory = tempfile::tempdir().unwrap();
    let (state, _) = admin_app(directory.path());
    admin_key(&state);
    let app = Router::new()
        .merge(komari_api::admin::admin_router())
        .with_state(state);

    let (status, _) = call(
        &app,
        bearer(Method::GET, "/api/admin/settings", Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
