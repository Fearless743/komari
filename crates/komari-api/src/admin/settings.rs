//! Settings read/write (`/api/admin/settings`).

use std::collections::HashMap;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use komari_auth::RequireAdmin;
use komari_config::keys;
use komari_web::SiteMeta;
use serde_json::Value;

use super::{audit, bad_request, internal, ok, parse_value};
use crate::response;
use crate::state::ApiState;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        // Go/Gin redirects the no-slash form before auth: 301 for GET/HEAD,
        // 307 for other methods (method and body preserved).
        .route("/api/admin/settings", any(redirect_settings))
        .route(
            "/api/admin/settings/",
            get(get_settings).post(edit_settings),
        )
}

fn settings_redirect(uri: &Uri, method: &Method) -> Response {
    let mut location = String::from(uri.path());
    location.push('/');
    if let Some(query) = uri.query() {
        location.push('?');
        location.push_str(query);
    }
    let status = if matches!(*method, Method::GET | Method::HEAD) {
        StatusCode::MOVED_PERMANENTLY
    } else {
        StatusCode::TEMPORARY_REDIRECT
    };
    let mut response = status.into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&location).expect("settings redirect location is valid"),
    );
    response
}

async fn redirect_settings(uri: Uri, method: Method) -> Response {
    settings_redirect(&uri, &method)
}

/// Redirect `/api/admin/settings` before the auth layers run, matching Gin's
/// `RedirectTrailingSlash` behaviour (unauthenticated callers see 301/307).
pub(crate) async fn redirect_settings_middleware(request: Request, next: Next) -> Response {
    if request.uri().path() == "/api/admin/settings" {
        return settings_redirect(request.uri(), request.method());
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

    if let Some(provider) = values.get(keys::OAUTH_PROVIDER_KEY).and_then(Value::as_str) {
        let name = if provider.is_empty() || provider == "none" {
            "github"
        } else {
            provider
        };
        match state.db.oauth().get(name) {
            Ok(Some(row)) => match komari_oauth::load_provider_global(name, &row.addition).await {
                Ok(provider) => state.set_oauth_provider(Some(provider)),
                Err(error) => {
                    tracing::warn!(%error, "failed to reload OAuth provider after settings update");
                }
            },
            Ok(None) => {
                tracing::warn!(provider = name, "OAuth provider row not found");
            }
            Err(error) => {
                tracing::warn!(%error, "failed to read OAuth provider after settings update");
            }
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
