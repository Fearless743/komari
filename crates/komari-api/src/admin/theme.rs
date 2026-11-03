//! Theme listing and switching (`/api/admin/theme/**`).
//!
//! Theme metadata and availability come from [`komari_web::WebAssets`], the
//! same handle the static router uses, so `try_set_theme` takes effect without
//! a restart.

use std::fs;
use std::path::PathBuf;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use komari_auth::RequireAdmin;
use komari_config::keys::THEME_KEY;
use serde_json::{Value, json};

use super::{audit, bad_request, internal, not_found, ok, parse_value};
use crate::response;
use crate::state::ApiState;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/admin/theme/list", get(list_themes))
        .route("/api/admin/theme/set", get(set_theme))
        .route("/api/admin/theme/delete", post(delete_theme))
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
    let request: DeleteThemeBody = match super::parse_json(&body, "参数错误: ") {
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

fn theme_directory(state: &ApiState, short: &str) -> PathBuf {
    state.data_dir().join(komari_web::THEMES_DIR).join(short)
}
