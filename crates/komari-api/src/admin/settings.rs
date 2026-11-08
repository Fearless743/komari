//! Settings read/write (`/api/admin/settings`).

use std::collections::HashMap;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use komari_auth::RequireAdmin;
use komari_config::keys;
use komari_web::SiteMeta;
use serde_json::Value;

use super::{audit, bad_request, internal, ok, parse_value};
use crate::response;
use crate::state::ApiState;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        // Gin's RedirectTrailingSlash only redirects GET (301) and POST (307).
        // HEAD and all other methods are 404, including on the trailing-slash
        // path. `MethodFilter::GET` deliberately excludes HEAD (unlike `get()`).
        .route("/api/admin/settings", any(settings_no_slash_handler))
        .route("/api/admin/settings/", any(settings_slash_handler))
}

fn redirect_location(uri: &Uri) -> String {
    let mut location = String::from(uri.path());
    location.push('/');
    if let Some(query) = uri.query() {
        location.push('?');
        location.push_str(query);
    }
    location
}

fn settings_get_redirect(uri: &Uri) -> Response {
    let location = redirect_location(uri);
    // Go's http.Redirect writes this body for GET (including two newlines:
    // one from the body template and one from fmt.Fprintln).
    let body = format!("<a href=\"{location}\">Moved Permanently</a>.\n\n");
    let mut response = (StatusCode::MOVED_PERMANENTLY, body).into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&location).expect("settings redirect location is valid"),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

fn settings_post_redirect(uri: &Uri) -> Response {
    let location = redirect_location(uri);
    let mut response = StatusCode::TEMPORARY_REDIRECT.into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&location).expect("settings redirect location is valid"),
    );
    response
}

fn settings_not_found() -> Response {
    // Go's default 404 body and headers for methods Gin has no route for.
    let mut response = (StatusCode::NOT_FOUND, "404 page not found").into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn settings_slash_handler(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    method: Method,
    body: Bytes,
) -> Response {
    match method {
        Method::GET => get_settings(State(state), admin).await,
        Method::POST => edit_settings(State(state), admin, body).await,
        _ => settings_not_found(),
    }
}

fn settings_no_slash(uri: Uri, method: Method) -> Response {
    match method {
        Method::GET => settings_get_redirect(&uri),
        Method::POST => settings_post_redirect(&uri),
        _ => settings_not_found(),
    }
}

async fn settings_no_slash_handler(uri: Uri, method: Method) -> Response {
    settings_no_slash(uri, method)
}

/// Match Gin's redirect/404 matrix before the auth layers run:
/// GET no-slash -> 301, POST no-slash -> 307, everything else -> 404.
/// On the trailing-slash path only GET/POST continue to auth and handlers.
pub(crate) async fn redirect_settings_middleware(request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if path == "/api/admin/settings" {
        return settings_no_slash(request.uri().clone(), request.method().clone());
    }
    if path == "/api/admin/settings/" && !matches!(*request.method(), Method::GET | Method::POST) {
        return settings_not_found();
    }
    next.run(request).await
}

/// `GET /api/admin/settings` — Go `admin.GetSettings`.
pub(crate) async fn get_settings(State(state): State<ApiState>, _admin: RequireAdmin) -> Response {
    match state.db.config().get_all() {
        Ok(values) => {
            response::success(Value::Object(values.into_iter().collect())).into_response()
        }
        Err(error) => internal(format!("Failed to get settings: {error}")),
    }
}

/// `POST /api/admin/settings` — Go `admin.EditSettings`.
pub(crate) async fn edit_settings(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let value = match parse_value(&body, "Invalid or missing request body: ") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Value::Object(object) = value else {
        return bad_request("Invalid or missing request body: expected a JSON object");
    };

    let values: HashMap<String, Value> = object.into_iter().collect();
    if let Err(error) = state.db.config().set_many(&values) {
        return internal(format!("Failed to update settings: {error}"));
    }
    apply_side_effects(&state, &values).await;

    let mut message = String::from("update settings: ");
    for key in values.keys() {
        message.push_str(key);
        message.push_str(", ");
    }
    if message.len() > 2 {
        message.truncate(message.len() - 2);
    }
    audit(&state, &admin, &message, "info");
    ok()
}

/// Apply the runtime effects of the Go `config.Subscribe` callbacks.
///
/// The database write already succeeded; failures here are logged so the
/// caller still gets a success response, matching Go's asynchronous reloads.
async fn apply_side_effects(state: &ApiState, values: &HashMap<String, Value>) {
    if values.contains_key(keys::DDNS_PROVIDER_KEY) || values.contains_key(keys::DDNS_ENABLED_KEY) {
        if let Err(error) = state.ddns.initialize().await {
            tracing::warn!(%error, "failed to reload DDNS provider after settings update");
        }
    }

    if values.contains_key(keys::GEO_IP_PROVIDER_KEY) {
        if let Some(service) = state.geoip.as_ref() {
            if let Err(error) = service.initialize().await {
                tracing::warn!(%error, "failed to reload GeoIP provider after settings update");
            }
        }
    }

    if let Some(theme) = values.get(keys::THEME_KEY).and_then(Value::as_str) {
        if let Err(error) = state.web.try_set_theme(theme) {
            tracing::warn!(%error, theme, "failed to switch theme after settings update");
        }
    }

    if values.contains_key(keys::SITENAME_KEY)
        || values.contains_key(keys::DESCRIPTION_KEY)
        || values.contains_key(keys::CUSTOM_HEAD_KEY)
        || values.contains_key(keys::CUSTOM_BODY_KEY)
    {
        let mut meta = state.web.site_meta();
        if let Some(value) = values.get(keys::SITENAME_KEY).and_then(Value::as_str) {
            meta.sitename = value.to_owned();
        }
        if let Some(value) = values.get(keys::DESCRIPTION_KEY).and_then(Value::as_str) {
            meta.description = value.to_owned();
        }
        if let Some(value) = values.get(keys::CUSTOM_HEAD_KEY).and_then(Value::as_str) {
            meta.custom_head = value.to_owned();
        }
        if let Some(value) = values.get(keys::CUSTOM_BODY_KEY).and_then(Value::as_str) {
            meta.custom_body = value.to_owned();
        }
        state.web.set_site_meta(SiteMeta {
            sitename: meta.sitename,
            description: meta.description,
            custom_head: meta.custom_head,
            custom_body: meta.custom_body,
        });
    }
}
