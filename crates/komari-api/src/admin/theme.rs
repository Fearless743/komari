//! Theme management (`/api/admin/theme/**`).
//!
//! Theme metadata and availability come from [`komari_web::WebAssets`], the
//! same handle the static router uses, so `try_set_theme` takes effect without
//! a restart. Upload/update/import mirror Go's `api/admin/theme.go`; package
//! inspection and installation live in [`theme_package`].

use std::fs;
use std::path::PathBuf;

use axum::Router;
use axum::body::Bytes;
use axum::extract::multipart::{MultipartError, MultipartRejection};
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Query, Request, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use komari_auth::RequireAdmin;
use komari_config::keys::THEME_KEY;
use komari_web::{THEME_CONFIG_FILE, ThemeInfo};
use serde_json::{Value, json};

use super::theme_package;
use super::{audit, bad_request, internal, not_found, ok, parse_json, parse_value};
use crate::response;
use crate::state::ApiState;
use crate::theme_fetch::{THEME_ARCHIVE_LIMIT, github_repo};

/// Hard cap for an uploaded theme archive (64 MiB).
const MAX_THEME_UPLOAD_BYTES: usize = THEME_ARCHIVE_LIMIT;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/admin/theme/upload",
            // Axum's default body limit is 2 MB; theme archives can be larger.
            put(upload_theme).layer(DefaultBodyLimit::max(MAX_THEME_UPLOAD_BYTES)),
        )
        .route("/api/admin/theme/list", get(list_themes))
        .route("/api/admin/theme/set", get(set_theme))
        .route("/api/admin/theme/delete", post(delete_theme))
        .route("/api/admin/theme/update", post(update_theme))
        .route("/api/admin/theme/import", post(import_theme))
        .route("/api/admin/theme/settings", post(update_theme_settings))
}

/// `GET /api/admin/theme/list` — Go `admin.ListThemes`.
pub(crate) async fn list_themes(State(state): State<ApiState>, _admin: RequireAdmin) -> Response {
    let themes = state.web.list_themes();
    response::success(json!(themes)).into_response()
}

#[derive(Default, serde::Deserialize)]
pub(crate) struct ThemeQuery {
    #[serde(default)]
    theme: String,
}

/// `GET /api/admin/theme/set?theme=...` — Go `admin.SetTheme`.
pub(crate) async fn set_theme(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    Query(query): Query<ThemeQuery>,
) -> Response {
    if query.theme.is_empty() {
        return bad_request("主题名称不能为空");
    }
    if let Err(error) = state.web.try_set_theme(&query.theme) {
        return match error {
            komari_web::ThemeSwitchError::InvalidId(_) => bad_request("主题名称无效"),
            komari_web::ThemeSwitchError::NotInstalled(_) => not_found("主题不存在"),
        };
    }
    if let Err(error) = state.db.config().set(THEME_KEY, &query.theme) {
        return internal(format!("更新主题设置失败: {error}"));
    }
    audit(
        &state,
        &admin,
        &format!("set theme:{}", query.theme),
        "info",
    );
    response::success_message("主题设置成功", json!({"theme": query.theme})).into_response()
}

#[derive(serde::Deserialize)]
struct DeleteThemeBody {
    #[serde(default)]
    short: String,
}

/// `POST /api/admin/theme/delete` — Go `admin.DeleteTheme`.
pub(crate) async fn delete_theme(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let request: DeleteThemeBody = match parse_json(&body, "参数错误: ") {
        Ok(request) => request,
        Err(response) => return response,
    };
    if request.short.is_empty() {
        return bad_request("参数错误: missing short");
    }
    if request.short == komari_web::DEFAULT_THEME {
        return bad_request("默认主题不能删除");
    }
    if !komari_web::is_safe_theme_id(&request.short) {
        return bad_request("主题名称无效");
    }

    let directory = theme_directory(&state, &request.short);
    if !directory.exists() {
        return not_found("主题不存在");
    }
    if let Err(error) = fs::remove_dir_all(&directory) {
        return internal(format!("删除主题失败: {error}"));
    }
    audit(
        &state,
        &admin,
        &format!("delete theme:{}", request.short),
        "warn",
    );
    response::success_message("主题删除成功", Value::Null).into_response()
}

/// `PUT /api/admin/theme/upload` — Go `admin.UploadTheme`.
///
/// The Go handler reads the raw request body, while the bundled admin UI posts
/// `multipart/form-data` with a `file` field. Both are accepted; the response
/// envelope and error messages match Go.
pub(crate) async fn upload_theme(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    request: Request,
) -> Response {
    let is_multipart = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .to_ascii_lowercase()
                .starts_with("multipart/form-data")
        });

    let data = if is_multipart {
        match read_multipart(request, &state).await {
            Ok(data) => data,
            Err(response) => return response,
        }
    } else {
        match axum::body::to_bytes(request.into_body(), MAX_THEME_UPLOAD_BYTES).await {
            Ok(bytes) => bytes.to_vec(),
            Err(_) => return theme_too_large(),
        }
    };

    if data.is_empty() {
        return bad_request("请选择要上传的主题文件");
    }

    let data_dir = state.data_dir();
    match tokio::task::spawn_blocking(move || theme_package::install_theme(data, &data_dir)).await {
        Ok(Ok(info)) => {
            audit(
                &state,
                &admin,
                &format!("upload theme:{}", info.short),
                "warn",
            );
            response::success_message("主题上传成功", json!(info)).into_response()
        }
        Ok(Err(error)) => bad_request(error),
        Err(error) => internal(format!("解压主题失败: {error}")),
    }
}

#[derive(serde::Deserialize)]
struct UpdateThemeBody {
    #[serde(default)]
    short: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    git_owner: String,
    #[serde(default)]
    git_repo: String,
}

/// `POST /api/admin/theme/update` — Go `admin.UpdateTheme`.
pub(crate) async fn update_theme(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let request: UpdateThemeBody = match parse_json(&body, "参数错误: ") {
        Ok(request) => request,
        Err(response) => return response,
    };
    if request.short.is_empty() {
        return bad_request("参数错误: missing short");
    }
    if !komari_web::is_safe_theme_id(&request.short) {
        return not_found("主题不存在");
    }

    let theme_dir = theme_directory(&state, &request.short);
    let config_path = theme_dir.join(THEME_CONFIG_FILE);
    if !config_path.exists() {
        return not_found("主题不存在");
    }

    let existing = match fs::read(&config_path) {
        Ok(data) => data,
        Err(error) => return internal(format!("读取主题配置失败: {error}")),
    };
    let theme_info: ThemeInfo = match serde_json::from_slice(&existing) {
        Ok(info) => info,
        Err(error) => return internal(format!("读取主题配置失败: {error}")),
    };

    // Mode 1/4: retry the URL stored in the theme; a GitHub repository URL is
    // resolved to its latest release first. Download errors are ignored here so
    // the request can fall back to an explicit URL/repository, matching Go.
    let mut theme_data: Option<Vec<u8>> = None;
    if !theme_info.url.is_empty() {
        if let Some((owner, repo)) = github_repo(&theme_info.url) {
            if let Ok(url) = github_release_url(&state, &owner, &repo).await {
                if let Ok(data) = download(&state, &url).await {
                    theme_data = Some(data);
                }
            }
        } else if let Ok(data) = download(&state, &theme_info.url).await {
            theme_data = Some(data);
        }
    }

    if theme_data.is_none() {
        if !request.git_owner.is_empty() && !request.git_repo.is_empty() {
            // Mode 3: explicit GitHub repository.
            let url = match github_release_url(&state, &request.git_owner, &request.git_repo).await
            {
                Ok(url) => url,
                Err(error) => {
                    return bad_request(format!("从GitHub获取下载链接失败: {error}"));
                }
            };
            match download(&state, &url).await {
                Ok(data) => theme_data = Some(data),
                Err(error) => {
                    return bad_request(format!("从GitHub下载主题失败: {error}"));
                }
            }
        } else if !request.url.is_empty() {
            // Mode 2: explicit URL.
            if let Some((owner, repo)) = github_repo(&request.url) {
                let url = match github_release_url(&state, &owner, &repo).await {
                    Ok(url) => url,
                    Err(error) => {
                        return bad_request(format!("从GitHub获取下载链接失败: {error}"));
                    }
                };
                match download(&state, &url).await {
                    Ok(data) => theme_data = Some(data),
                    Err(error) => {
                        return bad_request(format!("从GitHub下载主题失败: {error}"));
                    }
                }
            } else {
                match download(&state, &request.url).await {
                    Ok(data) => theme_data = Some(data),
                    Err(error) => {
                        return bad_request(format!("从新URL下载主题失败: {error}"));
                    }
                }
            }
        }
    }

    let Some(data) = theme_data else {
        return bad_request("无法下载主题，请提供有效的URL或GitHub仓库信息");
    };

    let data_dir = state.data_dir();
    match tokio::task::spawn_blocking(move || theme_package::install_theme(data, &data_dir)).await {
        Ok(Ok(info)) => {
            audit(
                &state,
                &admin,
                &format!("update theme:{}", info.short),
                "warn",
            );
            response::success_message("主题更新成功", json!(info)).into_response()
        }
        Ok(Err(error)) => bad_request(error),
        Err(error) => internal(format!("解压主题失败: {error}")),
    }
}

#[derive(Default, serde::Deserialize)]
pub(crate) struct ImportQuery {
    #[serde(default)]
    preview: String,
}

#[derive(serde::Deserialize)]
struct ImportThemeBody {
    #[serde(default)]
    url: String,
}

/// `POST /api/admin/theme/import[?preview=true]` — Go `admin.ImportTheme`.
pub(crate) async fn import_theme(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    Query(query): Query<ImportQuery>,
    body: Bytes,
) -> Response {
    let request: ImportThemeBody = match parse_json(&body, "参数错误: ") {
        Ok(request) => request,
        Err(response) => return response,
    };
    if request.url.is_empty() {
        return bad_request("参数错误: missing url");
    }

    let download_url = if let Some((owner, repo)) = github_repo(&request.url) {
        match github_release_url(&state, &owner, &repo).await {
            Ok(url) => url,
            Err(error) => return bad_request(format!("从GitHub获取下载链接失败: {error}")),
        }
    } else {
        request.url
    };

    let data = match download(&state, &download_url).await {
        Ok(data) => data,
        Err(error) => return bad_request(format!("下载主题失败: {error}")),
    };

    if query.preview == "true" {
        let theme = match theme_package::peek_theme(&data) {
            Ok(theme) => theme,
            Err(error) => return bad_request(error),
        };
        let exists = theme_directory(&state, &theme.short).exists();
        return response::success(json!({
            "theme": theme,
            "exists": exists,
        }))
        .into_response();
    }

    // Peek first to learn the target directory and whether it is overwritten,
    // then install atomically.
    let peeked = match theme_package::peek_theme(&data) {
        Ok(theme) => theme,
        Err(error) => return bad_request(error),
    };
    let overwritten = theme_directory(&state, &peeked.short).exists();

    let data_dir = state.data_dir();
    match tokio::task::spawn_blocking(move || theme_package::install_theme(data, &data_dir)).await {
        Ok(Ok(theme)) => {
            audit(
                &state,
                &admin,
                &format!("import theme:{}", theme.short),
                "warn",
            );
            let message = if overwritten {
                "主题导入成功（已覆盖同名主题）"
            } else {
                "主题导入成功"
            };
            response::success_message(message, json!(theme)).into_response()
        }
        Ok(Err(error)) => bad_request(error),
        Err(error) => internal(format!("解压主题失败: {error}")),
    }
}

/// `POST /api/admin/theme/settings?theme=...` — Go `UpdateThemeSettings`.
pub(crate) async fn update_theme_settings(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
    Query(query): Query<ThemeQuery>,
    body: Bytes,
) -> Response {
    if query.theme.is_empty() {
        return bad_request("主题名称不能为空");
    }
    let value = match parse_value(&body, "参数错误: ") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Value::Object(object) = value else {
        return bad_request("参数错误: expected an object");
    };
    match state.db.themes().set(&query.theme, &Value::Object(object)) {
        Ok(()) => ok(),
        Err(error) => internal(format!("生成主题配置失败: {error}")),
    }
}

#[allow(clippy::result_large_err)] // `Response` is the handler's natural error type.
async fn read_multipart(request: Request, state: &ApiState) -> Result<Vec<u8>, Response> {
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(multipart_rejection)?;
    let mut data = Vec::new();
    let mut found = false;

    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        if field.name() != Some("file") {
            continue;
        }
        if let Some(name) = field.file_name() {
            if !name.to_ascii_lowercase().ends_with(".zip") {
                return Err(bad_request("只支持ZIP格式的主题文件"));
            }
        }
        found = true;
        while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
            if data.len().saturating_add(chunk.len()) > MAX_THEME_UPLOAD_BYTES {
                return Err(theme_too_large());
            }
            data.extend_from_slice(&chunk);
        }
        break;
    }

    if found {
        Ok(data)
    } else {
        Err(bad_request("请选择要上传的主题文件"))
    }
}

fn multipart_rejection(error: MultipartRejection) -> Response {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        theme_too_large()
    } else {
        bad_request(format!("读取上传文件失败: {}", error.body_text()))
    }
}

fn multipart_error(error: MultipartError) -> Response {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        theme_too_large()
    } else {
        bad_request(format!("读取上传文件失败: {error}"))
    }
}

fn theme_too_large() -> Response {
    response::error_status(
        StatusCode::PAYLOAD_TOO_LARGE,
        format!(
            "主题文件过大（超过 {} MiB）",
            MAX_THEME_UPLOAD_BYTES / (1024 * 1024)
        ),
    )
}

async fn download(state: &ApiState, url: &str) -> Result<Vec<u8>, String> {
    state
        .theme_fetcher
        .download(url)
        .await
        .map_err(|error| error.to_string())
}

async fn github_release_url(state: &ApiState, owner: &str, repo: &str) -> Result<String, String> {
    state
        .theme_fetcher
        .github_release(owner, repo)
        .await
        .map_err(|error| error.to_string())
}

fn theme_directory(state: &ApiState, short: &str) -> PathBuf {
    state.data_dir().join(komari_web::THEMES_DIR).join(short)
}
