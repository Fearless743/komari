mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode, header};
use common::{AUTO_KEY, app, call, request, set_auto_key};
use serde_json::json;

#[tokio::test]
async fn register_returns_uuid_and_token_for_valid_key() {
    let (state, app) = app();
    set_auto_key(&state, AUTO_KEY);

    let req = request(
        Method::POST,
        "/api/clients/register?name=node-1",
        Body::empty(),
    );
    let req = {
        let mut parts = req.into_parts();
        parts.0.headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {AUTO_KEY}").parse().unwrap(),
        );
        axum::http::Request::from_parts(parts.0, parts.1)
    };
    let (status, body) = call(&app, req).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
    assert_eq!(body["message"], "");
    let uuid = body["data"]["uuid"].as_str().expect("uuid");
    let token = body["data"]["token"].as_str().expect("token");
    assert_eq!(token.len(), 22);

    let client = state.db.clients().get(uuid).unwrap().expect("client row");
    assert_eq!(client.name, "Auto-node-1");
    assert_eq!(client.token, token);
}

#[tokio::test]
async fn register_generates_random_auto_name() {
    let (state, app) = app();
    set_auto_key(&state, AUTO_KEY);

    let request = || {
        let mut req = request(Method::POST, "/api/clients/register", Body::empty());
        req.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {AUTO_KEY}").parse().unwrap(),
        );
        req
    };

    let (first_status, first) = call(&app, request()).await;
    let (second_status, second) = call(&app, request()).await;
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(second_status, StatusCode::OK);
    assert_ne!(first["data"]["uuid"], second["data"]["uuid"]);
    assert_ne!(first["data"]["token"], second["data"]["token"]);

    let uuid = first["data"]["uuid"].as_str().unwrap();
    let client = state.db.clients().get(uuid).unwrap().unwrap();
    assert!(client.name.starts_with("Auto-"));
    assert_eq!(client.name.len(), "Auto-".len() + 8);
}

#[tokio::test]
async fn register_rejects_wrong_missing_and_short_keys() {
    let (state, app) = app();
    set_auto_key(&state, AUTO_KEY);

    let wrong = {
        let mut req = request(Method::POST, "/api/clients/register", Body::empty());
        req.headers_mut().insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_static("Bearer 0123456789abcdeX"),
        );
        req
    };
    let (status, body) = call(&app, wrong).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["status"], "error");
    assert_eq!(body["message"], "Invalid AutoDiscovery Key");

    let (status, _) = call(
        &app,
        request(Method::POST, "/api/clients/register", Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    set_auto_key(&state, "short");
    let mut req = request(Method::POST, "/api/clients/register", Body::empty());
    req.headers_mut().insert(
        header::AUTHORIZATION,
        header::HeaderValue::from_static("Bearer short"),
    );
    let (status, body) = call(&app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["message"], "Invalid AutoDiscovery Key");

    assert_eq!(state.db.clients().count().unwrap(), 0);
}

#[tokio::test]
async fn register_tokens_are_unique_across_same_name() {
    let (state, app) = app();
    set_auto_key(&state, AUTO_KEY);

    let make = || {
        let mut req = request(
            Method::POST,
            "/api/clients/register?name=same",
            Body::empty(),
        );
        req.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {AUTO_KEY}").parse().unwrap(),
        );
        req
    };
    let (_, first) = call(&app, make()).await;
    let (_, second) = call(&app, make()).await;

    assert_ne!(first["data"]["uuid"], second["data"]["uuid"]);
    assert_ne!(first["data"]["token"], second["data"]["token"]);
    assert_eq!(state.db.clients().count().unwrap(), 2);

    let all = state.db.clients().list().unwrap();
    assert!(all.iter().all(|client| client.name == "Auto-same"));
    let mut tokens: Vec<_> = all.iter().map(|client| client.token.clone()).collect();
    tokens.sort();
    tokens.dedup();
    assert_eq!(tokens.len(), 2);
    assert_eq!(
        json!(tokens.len()),
        json!(state.db.clients().count().unwrap())
    );
}
