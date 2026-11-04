mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, StatusCode};
use axum::routing::get;
use common::{app, call, request};
use komari_api::client_router;

#[tokio::test]
async fn client_router_composes_with_other_routes() {
    let (state, _) = app();

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(client_router())
        .with_state(state);

    let (status, body) = call(&app, request(Method::GET, "/health", Body::empty())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!("ok"));

    // `POST /api/clients/register` was removed: the route is gone, so the
    // composed router must answer 404.
    let (status, _) = call(
        &app,
        request(Method::POST, "/api/clients/register", Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
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
    // The auto-discovery route no longer exists, even under a prefix.
    assert_eq!(status, StatusCode::NOT_FOUND);
}
