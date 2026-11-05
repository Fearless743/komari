//! Provider configuration: DDNS, notification senders and OAuth/OIDC.
//!
//! GET responses never echo secrets: DDNS and notification additions go
//! through the crates' `redact_addition`, OAuth uses [`redact_oauth_addition`].
//! The database keeps the complete configuration.

use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::http::header::{HeaderValue, LOCATION, SET_COOKIE};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use komari_auth::RequireAdmin;
use komari_db::LocalTime;
use komari_db::models::Provider;
use komari_ddns::factory as ddns_factory;
use komari_ddns::redact_addition as redact_ddns;
use komari_notify::redact_addition as redact_notify;
use komari_notify::registry as notify_registry;
use serde_json::{Map, Value, json};

use super::{audit, bad_request, internal, not_found, ok, parse_json, set_cookie};
use crate::response;
use crate::state::ApiState;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/admin/settings/ddns", get(get_ddns).post(set_ddns))
        .route(
            "/api/admin/settings/message-sender",
            get(get_message_sender).post(set_message_sender),
        )
        .route("/api/admin/settings/oidc", get(get_oidc).post(set_oidc))
        .route(
            "/api/admin/settings/ddns/cloudflare/zones",
            post(cloudflare_zones),
        )
        .route(
            "/api/admin/settings/ddns/history",
            get(get_ddns_history).delete(delete_ddns_history),
        )
        .route("/api/admin/update/ddns", post(sync_ddns))
        .route("/api/admin/oauth2/bind", get(bind_oauth))
        .route("/api/admin/oauth2/unbind", post(unbind_oauth))
}

/// `GET /api/admin/oauth2/bind` — set the binding cookie and redirect.
pub(crate) async fn bind_oauth(admin: RequireAdmin) -> Response {
    let Some(uuid) = admin.0.user_uuid.as_deref() else {
        return bad_request("No user found");
    };
    let mut response = (StatusCode::FOUND, [(LOCATION, "/api/oauth")]).into_response();
    if let Ok(value) = HeaderValue::from_str(&set_cookie("binding_external_account", uuid, 3600)) {
        response.headers_mut().insert(SET_COOKIE, value);
    }
    response
}

/// `POST /api/admin/oauth2/unbind`.
pub(crate) async fn unbind_oauth(State(state): State<ApiState>, admin: RequireAdmin) -> Response {
    let Some(uuid) = admin.0.user_uuid.as_deref() else {
        return bad_request("No user found");
    };
    match state.db.accounts().unbind_sso(uuid) {
        Ok(_) => {
            audit(&state, &admin, "unbind external account", "info");
            ok()
        }
        Err(error) => internal(format!("Failed to unbind external account: {error}")),
    }
}

#[derive(Default, serde::Deserialize)]
pub(crate) struct ProviderQuery {
    #[serde(default)]
    provider: String,
}

#[derive(serde::Deserialize)]
pub(crate) struct ProviderBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    addition: String,
}

// ---------------------------------------------------------------------------
// DDNS
// ---------------------------------------------------------------------------

/// `GET /api/admin/settings/ddns` — redacted stored config or field templates.
pub(crate) async fn get_ddns(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
    Query(query): Query<ProviderQuery>,
) -> Response {
    if !query.provider.is_empty() {
        let provider = match state.db.ddns_providers().get(&query.provider) {
            Ok(Some(provider)) => provider,
            Ok(None) => {
                return not_found(format!("Provider not found: {}", query.provider));
            }
            Err(error) => return internal(error.to_string()),
        };
        let safe = redact_ddns(&provider.name, &provider.addition);
        return response::success(json!({
            "name": provider.name,
            "addition": safe.to_string(),
        }))
        .into_response();
    }

    let configs = ddns_factory().get_provider_configs();
    if configs.is_empty() {
        return not_found("No DDNS providers found");
    }
    response::success(json!(configs)).into_response()
}

/// `POST /api/admin/settings/ddns` — validate and store the full config.
pub(crate) async fn set_ddns(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let config: ProviderBody = match parse_json(&body, "Invalid configuration: ") {
        Ok(config) => config,
        Err(response) => return response,
    };
    if config.name.is_empty() {
        return bad_request("Provider name is required");
    }

    let factory = ddns_factory();
    if factory.get_constructor(&config.name).is_err() {
        return not_found(format!("Provider not found: {}", config.name));
    }
    let mut provider = match factory.create(&config.name) {
        Ok(provider) => provider,
        Err(error) => return internal(error.to_string()),
    };
    let configuration: Value = match serde_json::from_str(&config.addition) {
        Ok(configuration) => configuration,
        Err(error) => {
            return bad_request(format!("Invalid provider configuration JSON: {error}"));
        }
    };
    if let Err(error) = provider.set_configuration(configuration) {
        return bad_request(format!("Invalid provider configuration JSON: {error}"));
    }
    if let Err(error) = provider.init() {
        return bad_request(format!("Provider initialization failed: {error}"));
    }

    let row = Provider::new(config.name.clone(), config.addition.clone());
    if let Err(error) = state.db.ddns_providers().save(&row) {
        return internal(format!(
            "Failed to save DDNS provider configuration: {error}"
        ));
    }

    let current = state
        .db
        .config()
        .get_as::<String>(komari_config::keys::DDNS_PROVIDER_KEY)
        .ok()
        .flatten()
        .unwrap_or_default();
    if current == config.name {
        if let Err(error) = state.ddns.load_provider(&config.name, &config.addition) {
            return internal(format!("Failed to load DDNS provider: {error}"));
        }
    }

    audit(
        &state,
        &admin,
        &format!("set ddns provider:{}", config.name),
        "info",
    );
    response::success(json!({"message": "DDNS provider set successfully"})).into_response()
}

/// `POST /api/admin/update/ddns` — Go `admin.SyncDdns`.
pub(crate) async fn sync_ddns(State(state): State<ApiState>, admin: RequireAdmin) -> Response {
    let clients = match state.db.clients().list() {
        Ok(clients) => clients,
        Err(error) => {
            return internal(format!("Failed to load clients for DDNS sync: {error}"));
        }
    };

    let provider = state
        .db
        .config()
        .get_as::<String>(komari_config::keys::DDNS_PROVIDER_KEY)
        .ok()
        .flatten()
        .unwrap_or_default();
    if !provider.is_empty() && provider != "none" {
        if let Ok(Some(row)) = state.db.ddns_providers().get(&provider) {
            if let Err(error) = state.ddns.load_provider(&provider, &row.addition) {
                tracing::warn!(%error, "failed to load DDNS provider for manual sync");
            } else {
                let service = state.ddns.clone();
                tokio::spawn(async move {
                    service.sync_all(&clients, "admin", true).await;
                });
            }
        }
    }

    audit(&state, &admin, "sync ddns", "info");
    response::success(json!({"message": "DDNS sync started"})).into_response()
}

// ---------------------------------------------------------------------------
// Notification senders
// ---------------------------------------------------------------------------

/// `GET /api/admin/settings/message-sender` — redacted config or templates.
pub(crate) async fn get_message_sender(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
    Query(query): Query<ProviderQuery>,
) -> Response {
    if !query.provider.is_empty() {
        let provider = match state.db.message_senders().get(&query.provider) {
            Ok(Some(provider)) => provider,
            Ok(None) => {
                return not_found(format!("Provider not found: {}", query.provider));
            }
            Err(error) => return internal(error.to_string()),
        };
        let safe = redact_notify(&provider.name, &provider.addition);
        return response::success(json!({
            "name": provider.name,
            "addition": safe.to_string(),
        }))
        .into_response();
    }

    let configs = notify_registry().config_fields();
    if configs.is_empty() {
        return not_found("No message sender providers found");
    }
    response::success(json!(configs)).into_response()
}

/// `POST /api/admin/settings/message-sender`.
pub(crate) async fn set_message_sender(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let config: ProviderBody = match parse_json(&body, "Invalid configuration: ") {
        Ok(config) => config,
        Err(response) => return response,
    };
    if config.name.is_empty() {
        return bad_request("Provider name is required");
    }
    if notify_registry().get(&config.name).is_none() {
        return not_found(format!("Provider not found: {}", config.name));
    }

    let row = Provider::new(config.name.clone(), config.addition.clone());
    if let Err(error) = state.db.message_senders().save(&row) {
        return internal(format!(
            "Failed to save message sender provider configuration: {error}"
        ));
    }

    let current = state
        .db
        .config()
        .get_as::<String>(komari_config::keys::NOTIFICATION_METHOD_KEY)
        .ok()
        .flatten()
        .unwrap_or_else(|| "none".to_owned());
    if current == config.name {
        if let Err(error) = komari_notify::load_provider(&config.name, &config.addition).await {
            return internal(format!("Failed to load message sender provider: {error}"));
        }
    }

    audit(
        &state,
        &admin,
        &format!("set message sender provider:{}", config.name),
        "info",
    );
    response::success(json!({"message": "Message sender provider set successfully"}))
        .into_response()
}

// ---------------------------------------------------------------------------
// OAuth / OIDC
// ---------------------------------------------------------------------------

/// `GET /api/admin/settings/oidc` — redacted config or field templates.
pub(crate) async fn get_oidc(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
    Query(query): Query<ProviderQuery>,
) -> Response {
    if !query.provider.is_empty() {
        let provider = match state.db.oauth().get(&query.provider) {
            Ok(Some(provider)) => provider,
            Ok(None) => {
                return not_found(format!("Provider not found: {}", query.provider));
            }
            Err(error) => return internal(error.to_string()),
        };
        let safe = redact_oauth_addition(&provider.addition);
        return response::success(json!({
            "name": provider.name,
            "addition": safe.to_string(),
        }))
        .into_response();
    }

    let mut providers = Map::new();
    for meta in komari_oauth::BUILTIN_METADATA {
        let fields = meta
            .fields
            .iter()
            .map(|field| {
                json!({
                    "name": field.name,
                    "required": field.required,
                    "type": "string",
                    "options": "",
                    "default": field.default.unwrap_or(""),
                    "help": field.description,
                })
            })
            .collect::<Vec<_>>();
        providers.insert(meta.name.to_owned(), Value::Array(fields));
    }
    if providers.is_empty() {
        return not_found("No OIDC providers found");
    }
    response::success(Value::Object(providers)).into_response()
}

/// `POST /api/admin/settings/oidc`.
pub(crate) async fn set_oidc(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let config: ProviderBody = match parse_json(&body, "Invalid configuration: ") {
        Ok(config) => config,
        Err(response) => return response,
    };
    if config.name.is_empty() {
        return bad_request("Provider name is required");
    }
    if komari_oauth::builtin_registry()
        .constructor(&config.name)
        .is_none()
    {
        return not_found(format!("Provider not found: {}", config.name));
    }

    let row = Provider::new(config.name.clone(), config.addition.clone());
    if let Err(error) = state.db.oauth().save(&row) {
        return internal(format!(
            "Failed to save OIDC provider configuration: {error}"
        ));
    }

    let current = state
        .db
        .config()
        .get_as::<String>(komari_config::keys::OAUTH_PROVIDER_KEY)
        .ok()
        .flatten()
        .unwrap_or_else(|| "github".to_owned());
    if current == config.name {
        match komari_oauth::load_provider_global(&config.name, &config.addition).await {
            Ok(provider) => state.set_oauth_provider(Some(provider)),
            Err(error) => {
                return internal(format!("Failed to load OIDC provider: {error}"));
            }
        }
    }

    audit(
        &state,
        &admin,
        &format!("set oidc provider:{}", config.name),
        "info",
    );
    response::success(json!({"message": "OIDC provider set successfully"})).into_response()
}

/// Remove secret fields from an OAuth `addition` JSON blob.
///
/// Keys are compared ignoring case, underscores and dashes, so `client_secret`,
/// `clientSecret` and `client-secret` are all removed.
pub(crate) fn redact_oauth_addition(addition: &str) -> Value {
    let Ok(Value::Object(mut object)) = serde_json::from_str::<Value>(addition) else {
        return Value::Object(Map::new());
    };
    object.retain(|key, _| !komari_oauth::is_sensitive_key(key));
    Value::Object(object)
}

// ---------------------------------------------------------------------------
// Cloudflare zones / DDNS history
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct ZonesBody {
    #[serde(default)]
    token: String,
}

/// `POST /api/admin/settings/ddns/cloudflare/zones` — Go `GetCloudflareZones`.
///
/// GET responses redact `api_token` (D-009), so the admin UI cannot send it
/// back for an already-configured provider. An empty request token therefore
/// falls back to the token stored in `ddns_providers`.
pub(crate) async fn cloudflare_zones(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
    body: Bytes,
) -> Response {
    let request: ZonesBody = match parse_json(&body, "Invalid request") {
        Ok(request) => request,
        Err(response) => return response,
    };
    let token = match normalize_cloudflare_token(&request.token) {
        token if !token.is_empty() => token,
        _ => match stored_cloudflare_token(&state) {
            Some(token) => token,
            None => {
                return bad_request(
                    "Token is required. Configure and save the Cloudflare API token first.",
                );
            }
        },
    };

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(error) => return internal(error.to_string()),
    };
    let response = client
        .get("https://api.cloudflare.com/client/v4/zones?status=active&per_page=50")
        .bearer_auth(&token)
        .header("Content-Type", "application/json")
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(_) => return internal("Failed to connect to Cloudflare API"),
    };
    if response.status() != reqwest::StatusCode::OK {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return internal(format!(
            "Cloudflare API Error: status {status}{}",
            cloudflare_error_detail(&body)
        ));
    }
    let value: Value = match response.json().await {
        Ok(value) => value,
        Err(_) => return internal("Failed to parse Cloudflare API response"),
    };
    if value.get("success").and_then(Value::as_bool) != Some(true) {
        return internal("Cloudflare returned success=false");
    }
    let result = value
        .get("result")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    response::success(result).into_response()
}

/// Trim whitespace and an accidental `Bearer ` prefix from a pasted token.
fn normalize_cloudflare_token(raw: &str) -> String {
    let token = raw.trim();
    let token = if token
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("bearer "))
    {
        token[7..].trim()
    } else {
        token
    };
    token.to_owned()
}

/// Read the stored Cloudflare API token. GET responses never expose it, so the
/// zones endpoint reads the full value straight from the database row.
fn stored_cloudflare_token(state: &ApiState) -> Option<String> {
    let provider = state.db.ddns_providers().get("cloudflare").ok().flatten()?;
    let addition: Value = serde_json::from_str(&provider.addition).ok()?;
    let token = addition.get("api_token").and_then(Value::as_str)?;
    let token = normalize_cloudflare_token(token);
    (!token.is_empty()).then_some(token)
}

/// Summarize Cloudflare's `errors[].message` without echoing the token.
fn cloudflare_error_detail(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return String::new();
    };
    let messages = value
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| {
                    let message = error.get("message").and_then(Value::as_str)?;
                    match error.get("code").and_then(Value::as_i64) {
                        Some(code) => Some(format!("{message} (code {code})")),
                        None => Some(message.to_owned()),
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if messages.is_empty() {
        String::new()
    } else {
        format!(": {}", messages.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::{cloudflare_error_detail, normalize_cloudflare_token};

    #[test]
    fn cloudflare_tokens_are_trimmed_and_stripped_of_bearer() {
        assert_eq!(normalize_cloudflare_token("  token-1 \n"), "token-1");
        assert_eq!(normalize_cloudflare_token("Bearer token-2"), "token-2");
        assert_eq!(normalize_cloudflare_token("bearer   token-3"), "token-3");
        assert_eq!(normalize_cloudflare_token("   "), "");
    }

    #[test]
    fn cloudflare_errors_are_summarized_without_the_token() {
        let body =
            r#"{"success":false,"errors":[{"code":10000,"message":"Authentication error"}]}"#;
        assert_eq!(
            cloudflare_error_detail(body),
            ": Authentication error (code 10000)"
        );
        assert_eq!(cloudflare_error_detail("not json"), "");
    }
}

#[derive(Default, serde::Deserialize)]
pub(crate) struct HistoryQuery {
    #[serde(default)]
    limit: Option<String>,
    #[serde(default)]
    client_uuid: String,
    #[serde(default)]
    before_days: Option<String>,
}

/// `GET /api/admin/settings/ddns/history`.
pub(crate) async fn get_ddns_history(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let limit = query
        .limit
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let limit = if limit <= 0 || limit > 200 { 50 } else { limit };
    match state.db.ddns().history(&query.client_uuid, limit) {
        Ok(histories) => response::success(json!(histories)).into_response(),
        Err(error) => internal(format!("Failed to get DDNS sync history: {error}")),
    }
}

/// `DELETE /api/admin/settings/ddns/history`.
pub(crate) async fn delete_ddns_history(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let days = query
        .before_days
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let days = if days <= 0 { 30 } else { days };
    let before = LocalTime::now().add_seconds(-days.saturating_mul(86_400));
    match state.db.ddns().delete_history_before(before) {
        Ok(_) => {
            audit(&state, &admin, "clean ddns sync history", "info");
            response::success(json!({"message": "DDNS sync history cleaned"})).into_response()
        }
        Err(error) => internal(format!("Failed to delete DDNS sync history: {error}")),
    }
}
