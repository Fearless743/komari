//! `/api/login`, `/api/logout` and `/api/me`.
//!
//! Ports of Go `api/public/login.go` and `api/public/me.go`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use komari_auth::DEFAULT_SESSION_TTL_SECONDS;
use komari_config::keys;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::state::ApiState;

use super::{
    PeerAddress, SESSION_COOKIE, audit, client_ip, cookie_value, redirect, respond_error,
    respond_success, set_cookie, user_agent,
};

/// Failed login attempts per client IP: `(failures, window_start)`.
/// Blocks brute force after 5 failures in 5 minutes with `429`.
fn login_attempts() -> &'static Mutex<HashMap<String, (u32, Instant)>> {
    static ATTEMPTS: OnceLock<Mutex<HashMap<String, (u32, Instant)>>> = OnceLock::new();
    ATTEMPTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn login_key(headers: &HeaderMap, peer: Option<std::net::SocketAddr>) -> String {
    client_ip(headers, peer)
}

fn check_login_rate_limit(key: &str) -> bool {
    let mut attempts = login_attempts().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((count, start)) = attempts.get(key) {
        if start.elapsed() < Duration::from_secs(300) && *count >= 5 {
            return false;
        }
        if start.elapsed() >= Duration::from_secs(300) {
            attempts.remove(key);
        }
    }
    true
}

fn record_login_failure(key: &str) {
    let mut attempts = login_attempts().lock().unwrap_or_else(|e| e.into_inner());
    let entry = attempts
        .entry(key.to_owned())
        .or_insert((0, Instant::now()));
    if entry.1.elapsed() >= Duration::from_secs(300) {
        *entry = (1, Instant::now());
    } else {
        entry.0 += 1;
    }
}

fn clear_login_failures(key: &str) {
    if let Ok(mut attempts) = login_attempts().lock() {
        attempts.remove(key);
    }
}

#[derive(Debug, Default, Deserialize)]
struct LoginRequest {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default, rename = "2fa_code")]
    two_fa: String,
}

pub(crate) async fn login(
    State(state): State<ApiState>,
    headers: HeaderMap,
    PeerAddress(peer): PeerAddress,
    body: Bytes,
) -> Response {
    let disabled = state
        .db
        .config()
        .get_as::<bool>(keys::DISABLE_PASSWORD_LOGIN_KEY)
        .ok()
        .flatten()
        .unwrap_or(false);
    if disabled {
        return respond_error(StatusCode::FORBIDDEN, "Password login is disabled");
    }

    let data: LoginRequest = match serde_json::from_slice(&body) {
        Ok(data) => data,
        Err(error) => {
            return respond_error(
                StatusCode::BAD_REQUEST,
                format!("Invalid request body: {error}"),
            );
        }
    };
    if data.username.is_empty() || data.password.is_empty() {
        return respond_error(
            StatusCode::BAD_REQUEST,
            "Invalid request body: Username and password are required",
        );
    }

    let rate_key = login_key(&headers, peer);
    if !check_login_rate_limit(&rate_key) {
        return respond_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed login attempts; try again later",
        );
    }

    // Go's `accounts.CheckPassword` swallows lookup errors and reports bad
    // credentials, so a database failure must not leak a distinct 500.
    let Ok(Some(uuid)) = state
        .db
        .accounts()
        .check_password(&data.username, &data.password)
    else {
        record_login_failure(&rate_key);
        return respond_error(StatusCode::UNAUTHORIZED, "Invalid credentials");
    };

    // Transparently upgrade legacy SHA-256 rows to bcrypt on first login.
    if let Ok(Some(user)) = state.db.accounts().get_by_uuid(&uuid) {
        if !komari_auth::is_bcrypt_hash(&user.passwd) {
            let _ = state.db.accounts().set_password(&uuid, &data.password);
        }
    }

    let two_factor = state
        .db
        .accounts()
        .get_by_uuid(&uuid)
        .ok()
        .flatten()
        .map(|user| user.two_factor)
        .unwrap_or_default();
    if !two_factor.is_empty() {
        if data.two_fa.is_empty() {
            record_login_failure(&rate_key);
            return respond_error(StatusCode::UNAUTHORIZED, "2FA code is required");
        }
        let valid = komari_auth::verify_2fa(&state.db, &uuid, &data.two_fa).unwrap_or(false);
        if !valid {
            record_login_failure(&rate_key);
            return respond_error(StatusCode::UNAUTHORIZED, "Invalid 2FA code");
        }
    }

    let ip = client_ip(&headers, peer);
    let session =
        match komari_auth::create_session(&state.db, &uuid, &user_agent(&headers), &ip, "password")
        {
            Ok(session) => session,
            Err(error) => {
                tracing::warn!(%error, "failed to create session");
                return respond_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to create session",
                );
            }
        };

    clear_login_failures(&rate_key);

    audit(
        &state,
        &headers,
        peer,
        &uuid,
        "logged in (password)",
        "login",
    );

    let mut response = respond_success(json!({
        "set-cookie": { "session_token": session },
    }));
    set_cookie(
        &mut response,
        SESSION_COOKIE,
        &session,
        DEFAULT_SESSION_TTL_SECONDS,
    );
    response
}

pub(crate) async fn logout(
    State(state): State<ApiState>,
    headers: HeaderMap,
    PeerAddress(peer): PeerAddress,
) -> Response {
    if let Some(session) = cookie_value(&headers, SESSION_COOKIE) {
        if !session.is_empty() {
            if let Err(error) = komari_auth::delete_session(&state.db, &session) {
                tracing::warn!(%error, "failed to delete session");
            }
        }
    }
    audit(&state, &headers, peer, "", "logged out", "logout");

    let mut response = redirect("/");
    set_cookie(&mut response, SESSION_COOKIE, "", -1);
    response
}

pub(crate) async fn get_me(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let guest = || {
        (
            StatusCode::OK,
            Json(json!({ "username": "Guest", "logged_in": false })),
        )
            .into_response()
    };

    let Some(session) = cookie_value(&headers, SESSION_COOKIE) else {
        return guest();
    };
    if session.is_empty() {
        return guest();
    }

    // Go `GetMe` uses `accounts.GetSession`, which deletes expired rows.
    let Ok(Some(validation)) = komari_auth::validate_session(&state.db, &session) else {
        return guest();
    };
    let user = validation.user;

    (
        StatusCode::OK,
        Json(json!({
            "username": user.username,
            "logged_in": true,
            "uuid": user.uuid,
            "2fa_enabled": !user.two_factor.is_empty(),
        })),
    )
        .into_response()
}
