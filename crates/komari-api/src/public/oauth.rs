//! `/api/oauth` and `/api/oauth_callback`.
//!
//! Ports of Go `api/public/oauth.go` on top of the B13 provider abstraction.

use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use komari_auth::DEFAULT_SESSION_TTL_SECONDS;
use komari_config::keys;
use komari_oauth::{AccountLinker, CallbackQuery, CallbackRequest, SsoIdentity, redacted_error};

use crate::state::ApiState;

use super::{
    BINDING_COOKIE, OAUTH_STATE_COOKIE, PeerAddress, SESSION_COOKIE, audit, callback_url,
    client_ip, cookie_value, redirect, respond_error, respond_raw_error, set_cookie, user_agent,
};

pub(crate) async fn authorize(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let enabled = state
        .db
        .config()
        .get_as::<bool>(keys::OAUTH_ENABLED_KEY)
        .ok()
        .flatten()
        .unwrap_or(false);
    if !enabled {
        return respond_raw_error(StatusCode::FORBIDDEN, "OAuth is not enabled");
    }

    let Some(provider) = state.oauth_provider() else {
        return respond_raw_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "OAuth provider is not initialized",
        );
    };

    match provider.authorization_url(&callback_url(&headers)).await {
        Ok(request) => {
            let mut response = redirect(&request.url);
            set_cookie(&mut response, OAUTH_STATE_COOKIE, &request.state, 3600);
            response
        }
        Err(error) => respond_raw_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "Failed to get authorization URL: {}",
                redacted_error(&error, &[])
            ),
        ),
    }
}

pub(crate) async fn callback(
    State(state): State<ApiState>,
    headers: HeaderMap,
    PeerAddress(peer): PeerAddress,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let Some(provider) = state.oauth_provider() else {
        return respond_raw_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "OAuth provider is not initialized",
        );
    };

    let query = parse_query(raw_query.as_deref());
    let oauth_state = cookie_value(&headers, OAUTH_STATE_COOKIE);
    let Some(oauth_state) = oauth_state.as_deref().filter(|value| !value.is_empty()) else {
        return clear_oauth_cookies(respond_raw_error(StatusCode::BAD_REQUEST, "Invalid state"));
    };
    let query_state = query.get("state").map(String::as_str);
    if !provider.skip_query_state_check() && query_state != Some(oauth_state) {
        return clear_oauth_cookies(respond_raw_error(StatusCode::BAD_REQUEST, "Invalid state"));
    }

    let redirect_uri = callback_url(&headers);
    let request = CallbackRequest {
        state: oauth_state,
        query_state,
        code: query.get("code").map(String::as_str),
        redirect_uri: &redirect_uri,
        query: &query,
        headers: &headers,
    };
    let oauth_user = match provider.callback(request).await {
        Ok(user) => user,
        Err(error) => {
            return clear_oauth_cookies(respond_raw_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get user info: {}", redacted_error(&error, &[])),
            ));
        }
    };

    let identity = SsoIdentity::new(provider.name(), oauth_user.subject);
    let binding = cookie_value(&headers, BINDING_COOKIE).filter(|value| !value.is_empty());

    let response = if let Some(binding_uuid) = binding {
        // Binding flow: the cookie set by the admin endpoint names the account.
        let Some(session) = cookie_value(&headers, SESSION_COOKIE).filter(|v| !v.is_empty()) else {
            return clear_oauth_cookies(respond_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Binding failed",
            ));
        };
        let Ok(Some(user)) = komari_auth::session_user(&state.db, &session) else {
            return clear_oauth_cookies(respond_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Binding failed",
            ));
        };
        if user.uuid != binding_uuid {
            return clear_oauth_cookies(respond_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Binding failed",
            ));
        }
        if AccountLinker::new(&state.db)
            .bind(&user.uuid, &identity)
            .is_err()
        {
            return clear_oauth_cookies(respond_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Binding failed",
            ));
        }
        audit(
            &state,
            &headers,
            peer,
            &user.uuid,
            format!(
                "bound external account (OAuth),sso_id: {}",
                identity.combined_id()
            ),
            "login",
        );
        redirect("/admin")
    } else {
        let Ok(user) = AccountLinker::new(&state.db).login(&identity) else {
            return clear_oauth_cookies(respond_error(
                StatusCode::UNAUTHORIZED,
                "please log in and bind your external account first.",
            ));
        };
        let session = match komari_auth::create_session(
            &state.db,
            &user.uuid,
            &user_agent(&headers),
            &client_ip(&headers, peer),
            "oauth",
        ) {
            Ok(session) => session,
            Err(error) => {
                return clear_oauth_cookies(respond_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    error.to_string(),
                ));
            }
        };
        audit(
            &state,
            &headers,
            peer,
            &user.uuid,
            "logged in (OAuth)",
            "login",
        );
        let mut response = redirect("/admin");
        set_cookie(
            &mut response,
            SESSION_COOKIE,
            &session,
            DEFAULT_SESSION_TTL_SECONDS,
        );
        response
    };

    clear_oauth_cookies(response)
}

fn clear_oauth_cookies(mut response: Response) -> Response {
    set_cookie(&mut response, OAUTH_STATE_COOKIE, "", -1);
    set_cookie(&mut response, BINDING_COOKIE, "", -1);
    response
}

/// Parse the callback query, keeping the first value of duplicate keys (Go
/// `c.Request.URL.Query()` uses `values[0]`).
fn parse_query(raw: Option<&str>) -> CallbackQuery {
    let mut query = CallbackQuery::new();
    if let Some(raw) = raw {
        for (key, value) in form_urlencoded::parse(raw.as_bytes()) {
            query
                .entry(key.into_owned())
                .or_insert_with(|| value.into_owned());
        }
    }
    query
}
