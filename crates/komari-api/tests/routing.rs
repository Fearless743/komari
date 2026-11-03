mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, StatusCode};
use axum::routing::get;
use common::{AUTO_KEY, app, call, request, set_auto_key};
use komari_api::client_router;

#[tokio::test]
async fn client_router_composes_with_other_routes() {
    let (state, _) = app();
    set_auto_key(&state, AUTO_KEY);

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(client_router())
        .with_state(state);

    let (status, body) = call(&app, request(Method::GET, "/health", Body::empty())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!("ok"));

    let mut register = request(Method::POST, "/api/clients/register", Body::empty());
    register.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {AUTO_KEY}").parse().unwrap(),
    );
    let (status, body) = call(&app, register).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
}

#[tokio::test]
async fn client_router_can_be_nested() {
    let (state, _) = app();
    let app = Router::new()
        .nest("/prefix", client_router())
        .with_state(state);

    let (status, _) = call(
        &app,
        request(Method::POST, "/prefix/api/clients/register", Body::empty()),
    )
    .await;
    // No auto-discovery key configured yet, so the nested route is reachable
    // and rejects the request rather than returning 404.
    assert_eq!(status, StatusCode::FORBIDDEN);
}
