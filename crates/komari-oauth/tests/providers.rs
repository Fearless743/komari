//! End-to-end provider tests using local HTTP mocks.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, HeaderValue};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use komari_db::Db;
use komari_oauth::{
    AccountLinker, CallbackQuery, CallbackRequest, ExchangeRequest, OAuthError, OAuthManager,
    SsoIdentity, factory,
};
use rsa::RsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::traits::PublicKeyParts;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Captured = Arc<Mutex<Vec<HashMap<String, String>>>>;

async fn spawn(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

fn query_of(url: &str) -> HashMap<String, String> {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn callback<'a>(
    state: &'a str,
    query_state: Option<&'a str>,
    code: Option<&'a str>,
    redirect_uri: &'a str,
    query: &'a CallbackQuery,
    headers: &'a HeaderMap,
) -> CallbackRequest<'a> {
    CallbackRequest {
        state,
        query_state,
        code,
        redirect_uri,
        query,
        headers,
    }
}

fn b64(bytes: impl AsRef<[u8]>) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn sign_jwt(key: &RsaPrivateKey, kid: &str, claims: Value) -> String {
    let header = json!({"alg": "RS256", "typ": "JWT", "kid": kid});
    let header = b64(serde_json::to_vec(&header).unwrap());
    let payload = b64(serde_json::to_vec(&claims).unwrap());
    let input = format!("{header}.{payload}");
    let signing_key = SigningKey::<Sha256>::new(key.clone());
    let signature = signing_key.sign(input.as_bytes());
    format!("{input}.{}", b64(signature.to_vec()))
}

fn jwks(key: &RsaPrivateKey, kid: &str) -> Value {
    let public = rsa::RsaPublicKey::from(key);
    json!({
        "keys": [{
            "kty": "RSA",
            "kid": kid,
            "use": "sig",
            "alg": "RS256",
            "n": b64(public.n().to_bytes_be()),
            "e": b64(public.e().to_bytes_be()),
        }]
    })
}

#[tokio::test]
async fn factory_exposes_the_go_provider_names_and_fields() {
    let names = factory::all_provider_names();
    assert!(names.contains(&"generic"));
    assert!(names.contains(&"github"));
    assert!(names.contains(&"qq"));
    assert!(names.contains(&"CloudflareAccess"));

    let registry = factory::builtin_registry();
    let generic = registry.config_fields("generic").unwrap();
    let field_names = generic.iter().map(|field| field.name).collect::<Vec<_>>();
    assert_eq!(
        field_names,
        [
            "client_id",
            "client_secret",
            "auth_url",
            "token_url",
            "user_info_url",
            "scope",
            "user_id_field"
        ]
    );
    assert!(
        generic
            .iter()
            .any(|field| field.name == "client_secret" && field.secret)
    );
    let qq = registry.config_fields("qq").unwrap();
    assert_eq!(qq[0].default, Some("https://login.qjqq.cn"));
}

#[tokio::test]
async fn generic_authorization_url_matches_go_parameters() {
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "auth_url": "https://idp.example/authorize",
                "token_url": "https://idp.example/token",
                "user_info_url": "https://idp.example/userinfo",
                "scope": "openid profile",
                "user_id_field": "sub",
            }),
        )
        .await
        .unwrap();

    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    assert_eq!(request.state.len(), 16);
    assert!(request.url.starts_with("https://idp.example/authorize?"));
    let query = query_of(&request.url);
    assert_eq!(query.get("client_id").unwrap(), "client-id");
    assert_eq!(query.get("state").unwrap(), &request.state);
    assert_eq!(query.get("scope").unwrap(), "openid profile");
    assert_eq!(query.get("redirect_uri").unwrap(), redirect);
    assert_eq!(query.get("response_type").unwrap(), "code");
    assert!(!query.contains_key("code_challenge"));
}

#[tokio::test]
async fn generic_pkce_sends_a_challenge_and_the_verifier() {
    let captured = Arc::new(Mutex::new(Vec::<HashMap<String, String>>::new()));
    let app =
        Router::new()
            .route(
                "/token",
                post(
                    |State(captured): State<Captured>,
                     Form(form): Form<HashMap<String, String>>| async move {
                        captured.lock().unwrap().push(form);
                        Json(json!({"access_token": "pkce-token", "token_type": "Bearer"}))
                    },
                ),
            )
            .with_state(Arc::clone(&captured));
    let base = spawn(app).await;

    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "auth_url": "https://idp.example/authorize",
                "token_url": format!("{base}/token"),
                "user_info_url": "https://idp.example/userinfo",
                "scope": "openid",
                "user_id_field": "sub",
                "use_pkce": true,
            }),
        )
        .await
        .unwrap();
    let request = provider
        .authorization_url("https://monitor.example/api/oauth_callback")
        .await
        .unwrap();
    let query = query_of(&request.url);
    let challenge = query.get("code_challenge").unwrap();
    assert_eq!(
        query.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    let verifier = request.pkce_verifier.clone().unwrap();
    let expected = b64(sha2::Sha256::digest(verifier.as_bytes()));
    assert_eq!(challenge, &expected);

    let callback_query = CallbackQuery::new();
    provider
        .exchange_code(ExchangeRequest {
            code: "code",
            redirect_uri: "https://monitor.example/api/oauth_callback",
            pkce_verifier: Some(&verifier),
            nonce: None,
            query: &callback_query,
        })
        .await
        .unwrap();
    let calls = captured.lock().unwrap();
    assert_eq!(calls[0].get("code_verifier").unwrap(), &verifier);
}

#[tokio::test]
async fn github_authorization_url_matches_go_parameters() {
    let provider = factory::builtin_registry()
        .create_and_init(
            "github",
            json!({"client_id": "gh-client", "client_secret": "gh-secret"}),
        )
        .await
        .unwrap();
    let request = provider
        .authorization_url("https://monitor.example/api/oauth_callback")
        .await
        .unwrap();
    assert!(
        request
            .url
            .starts_with("https://github.com/login/oauth/authorize?")
    );
    let query = query_of(&request.url);
    assert_eq!(query.get("client_id").unwrap(), "gh-client");
    assert_eq!(query.get("scope").unwrap(), "user:email");
    assert_eq!(query.get("state").unwrap(), &request.state);
    assert!(!query.contains_key("redirect_uri"));
}

#[tokio::test]
async fn qq_authorization_url_uses_the_aggregation_login_endpoint() {
    let captured = Arc::new(Mutex::new(Vec::<HashMap<String, String>>::new()));
    let app = Router::new()
        .route(
            "/connect.php",
            get(
                |State(captured): State<Captured>,
                 Query(query): Query<HashMap<String, String>>| async move {
                    captured.lock().unwrap().push(query);
                    Json(json!({"code": 0, "msg": "ok", "url": "https://login.example/authorize?x=1"}))
                },
            ),
        )
        .with_state(Arc::clone(&captured));
    let base = spawn(app).await;

    let provider = factory::builtin_registry()
        .create_and_init(
            "qq",
            json!({
                "aggregation_url": base,
                "app_id": "app-id",
                "app_key": "app-key",
                "login_type": "qq",
            }),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    assert_eq!(request.url, "https://login.example/authorize?x=1");
    assert_eq!(request.state.len(), 16);

    let calls = captured.lock().unwrap();
    let login = calls.first().unwrap();
    assert_eq!(login.get("act").unwrap(), "login");
    assert_eq!(login.get("appid").unwrap(), "app-id");
    assert_eq!(login.get("appkey").unwrap(), "app-key");
    assert_eq!(login.get("type").unwrap(), "qq");
    assert_eq!(login.get("redirect_uri").unwrap(), redirect);
}

#[tokio::test]
async fn cloudflare_authorization_url_appends_an_eight_character_state() {
    let provider = factory::builtin_registry()
        .create_and_init(
            "CloudflareAccess",
            json!({"team_domain": "team", "policy_aud": "aud"}),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback?foo=bar";
    let request = provider.authorization_url(redirect).await.unwrap();
    assert!(request.url.starts_with(redirect));
    assert!(request.url.contains("&state="));
    assert_eq!(request.state.len(), 8);
}

#[tokio::test]
async fn callback_rejects_state_mismatch_and_redirect_mismatch() {
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "auth_url": "https://idp.example/authorize",
                "token_url": "https://idp.example/token",
                "user_info_url": "https://idp.example/userinfo",
                "scope": "openid",
                "user_id_field": "sub",
            }),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    let query = CallbackQuery::new();
    let headers = HeaderMap::new();

    let error = provider
        .callback(callback(
            "not-the-state",
            Some("not-the-state"),
            Some("code"),
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, OAuthError::InvalidState));

    let error = provider
        .callback(callback(
            &request.state,
            Some(&request.state),
            Some("code"),
            "https://evil.example/callback",
            &query,
            &headers,
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, OAuthError::RedirectUriMismatch));

    assert!(matches!(
        provider
            .authorization_url("ftp://monitor.example/callback")
            .await,
        Err(OAuthError::InvalidRedirectUri(_))
    ));
}

#[tokio::test]
async fn callback_surfaces_provider_error_parameters() {
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "auth_url": "https://idp.example/authorize",
                "token_url": "https://idp.example/token",
                "user_info_url": "https://idp.example/userinfo",
                "scope": "openid",
                "user_id_field": "sub",
            }),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    let mut query = CallbackQuery::new();
    query.insert("error".to_owned(), "access_denied".to_owned());
    query.insert(
        "error_description".to_owned(),
        "the user denied the request".to_owned(),
    );
    let headers = HeaderMap::new();

    let error = provider
        .callback(callback(
            &request.state,
            Some(&request.state),
            None,
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap_err();
    match error {
        OAuthError::ProviderError {
            code, description, ..
        } => {
            assert_eq!(code, "access_denied");
            assert!(description.contains("denied"));
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[tokio::test]
async fn github_exchanges_tokens_and_falls_back_to_the_emails_api() {
    let captured = Arc::new(Mutex::new(Vec::<HashMap<String, String>>::new()));
    let app = Router::new()
        .route(
            "/token",
            post(
                |State(captured): State<Captured>,
                 Query(query): Query<HashMap<String, String>>| async move {
                    captured.lock().unwrap().push(query);
                    Json(json!({"access_token": "gh-token", "token_type": "bearer"}))
                },
            ),
        )
        .route(
            "/user",
            get(|| async {
                Json(json!({"id": 42, "login": "octocat", "email": null, "avatar_url": "https://avatar"}))
            }),
        )
        .route(
            "/emails",
            get(|| async {
                Json(json!([
                    {"email": "secondary@example.com", "primary": false, "verified": true},
                    {"email": "primary@example.com", "primary": true, "verified": true}
                ]))
            }),
        )
        .with_state(Arc::clone(&captured));
    let base = spawn(app).await;

    let provider = factory::builtin_registry()
        .create_and_init(
            "github",
            json!({
                "client_id": "gh-client",
                "client_secret": "gh-secret",
                "token_url": format!("{base}/token"),
                "user_api_url": format!("{base}/user"),
                "email_api_url": format!("{base}/emails"),
            }),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    let mut query = CallbackQuery::new();
    query.insert("code".to_owned(), "the-code".to_owned());
    let headers = HeaderMap::new();

    let user = provider
        .callback(callback(
            &request.state,
            Some(&request.state),
            Some("the-code"),
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap();
    assert_eq!(user.provider, "github");
    assert_eq!(user.subject, "42");
    assert_eq!(user.username.as_deref(), Some("octocat"));
    assert_eq!(user.email.as_deref(), Some("primary@example.com"));

    let calls = captured.lock().unwrap();
    let token_request = calls.first().unwrap();
    assert_eq!(token_request.get("client_id").unwrap(), "gh-client");
    assert_eq!(token_request.get("client_secret").unwrap(), "gh-secret");
    assert_eq!(token_request.get("code").unwrap(), "the-code");
}

#[tokio::test]
async fn qq_callback_maps_social_uid_and_nickname() {
    let captured = Arc::new(Mutex::new(Vec::<HashMap<String, String>>::new()));
    let app = Router::new()
        .route(
            "/connect.php",
            get(
                |State(captured): State<Captured>,
                 Query(query): Query<HashMap<String, String>>| async move {
                    let action = query.get("act").cloned().unwrap_or_default();
                    captured.lock().unwrap().push(query);
                    if action == "login" {
                        Json(json!({"code": 0, "msg": "ok", "url": "https://login.example/auth"}))
                    } else {
                        Json(json!({
                            "code": 0,
                            "msg": "ok",
                            "social_uid": "qq-uid",
                            "nickname": "QQ User",
                            "faceimg": "https://face",
                            "openid": "open-id",
                            "unionid": "union-id",
                            "access_token": "qq-access-token"
                        }))
                    }
                },
            ),
        )
        .with_state(Arc::clone(&captured));
    let base = spawn(app).await;

    let provider = factory::builtin_registry()
        .create_and_init(
            "qq",
            json!({
                "aggregation_url": base,
                "app_id": "app-id",
                "app_key": "app-key",
                "login_type": "qq",
            }),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    let mut query = CallbackQuery::new();
    query.insert("code".to_owned(), "qq-code".to_owned());
    query.insert("type".to_owned(), "qq".to_owned());
    let headers = HeaderMap::new();

    let user = provider
        .callback(callback(
            &request.state,
            None,
            Some("qq-code"),
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap();
    assert_eq!(user.subject, "qq-uid");
    assert_eq!(user.username.as_deref(), Some("QQ User"));
    assert_eq!(user.union_id.as_deref(), Some("union-id"));
    assert_eq!(user.raw.as_ref().unwrap()["access_token"], "********");

    let calls = captured.lock().unwrap();
    let callback = calls.last().unwrap();
    assert_eq!(callback.get("act").unwrap(), "callback");
    assert_eq!(callback.get("appid").unwrap(), "app-id");
    assert_eq!(callback.get("appkey").unwrap(), "app-key");
    assert_eq!(callback.get("type").unwrap(), "qq");
    assert_eq!(callback.get("code").unwrap(), "qq-code");
}

#[tokio::test]
async fn qq_callback_reports_aggregation_error_codes() {
    let app = Router::new().route(
        "/connect.php",
        get(|Query(query): Query<HashMap<String, String>>| async move {
            if query.get("act").map(String::as_str) == Some("login") {
                Json(json!({"code": 0, "msg": "ok", "url": "https://login.example/auth"}))
            } else {
                Json(json!({"code": 100, "msg": "invalid code"}))
            }
        }),
    );
    let base = spawn(app).await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "qq",
            json!({
                "aggregation_url": base,
                "app_id": "app-id",
                "app_key": "app-key",
                "login_type": "qq",
            }),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    let mut query = CallbackQuery::new();
    query.insert("code".to_owned(), "bad".to_owned());
    let headers = HeaderMap::new();

    let error = provider
        .callback(callback(
            &request.state,
            None,
            Some("bad"),
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("100"));
    assert!(message.contains("invalid code"));
}

/// Start a mock OIDC server. `token_audience` lets a test produce an
/// `id_token` with the wrong audience.
async fn oidc_server(token_audience: &str) -> (String, Arc<RsaPrivateKey>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}");
    let key = Arc::new(RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap());
    let key_set = jwks(&key, "test-key");
    let token_audience = token_audience.to_owned();

    let discovery_base = base.clone();
    let token_base = base.clone();
    let signing_key = Arc::clone(&key);
    let app = Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(move || {
                let base = discovery_base.clone();
                async move {
                    Json(json!({
                        "issuer": base,
                        "authorization_endpoint": format!("{base}/authorize"),
                        "token_endpoint": format!("{base}/token"),
                        "userinfo_endpoint": format!("{base}/userinfo"),
                        "jwks_uri": format!("{base}/jwks"),
                    }))
                }
            }),
        )
        .route(
            "/jwks",
            get(move || {
                let key_set = key_set.clone();
                async move { Json(key_set) }
            }),
        )
        .route(
            "/token",
            post(move |Form(form): Form<HashMap<String, String>>| {
                let base = token_base.clone();
                let key = Arc::clone(&signing_key);
                let audience = token_audience.clone();
                async move {
                    if form.get("grant_type").map(String::as_str) == Some("refresh_token") {
                        return Json(json!({
                            "access_token": "refreshed-token",
                            "token_type": "Bearer",
                            "expires_in": 3600
                        }));
                    }
                    let id_token = sign_jwt(
                        &key,
                        "test-key",
                        json!({
                            "iss": base,
                            "aud": audience,
                            "sub": "oidc-user",
                            "exp": chrono::Utc::now().timestamp() + 600,
                        }),
                    );
                    Json(json!({
                        "access_token": "oidc-access-token",
                        "token_type": "Bearer",
                        "expires_in": 3600,
                        "refresh_token": "refresh-token",
                        "id_token": id_token
                    }))
                }
            }),
        )
        .route(
            "/userinfo",
            get(|| async {
                Json(json!({
                    "sub": "oidc-user",
                    "name": "Alice",
                    "email": "alice@example.com",
                    "email_verified": true
                }))
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, key)
}

#[tokio::test]
async fn generic_oidc_discovers_jwks_and_validates_the_id_token() {
    let (base, _key) = oidc_server("client-id").await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "issuer": base,
                "user_id_field": "sub",
                "id_token_required": true,
            }),
        )
        .await
        .unwrap();

    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    assert!(request.url.starts_with(&format!("{base}/authorize?")));
    assert_eq!(request.state.len(), 16);

    let mut query = CallbackQuery::new();
    query.insert("code".to_owned(), "code".to_owned());
    let headers = HeaderMap::new();
    let user = provider
        .callback(callback(
            &request.state,
            Some(&request.state),
            Some("code"),
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap();
    assert_eq!(user.subject, "oidc-user");
    assert_eq!(user.username.as_deref(), Some("Alice"));
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
}

#[tokio::test]
async fn generic_oidc_rejects_an_id_token_with_the_wrong_audience() {
    let (base, _key) = oidc_server("some-other-client").await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "issuer": base,
                "user_id_field": "sub",
                "id_token_required": true,
            }),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    let query = CallbackQuery::new();
    let headers = HeaderMap::new();

    let error = provider
        .callback(callback(
            &request.state,
            Some(&request.state),
            Some("code"),
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, OAuthError::InvalidToken(_)));
}

#[tokio::test]
async fn generic_refresh_returns_a_new_token_with_an_expiry() {
    let (base, _key) = oidc_server("client-id").await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "issuer": base,
                "user_id_field": "sub",
            }),
        )
        .await
        .unwrap();
    let query = CallbackQuery::new();
    let tokens = provider
        .exchange_code(ExchangeRequest {
            code: "code",
            redirect_uri: "https://monitor.example/api/oauth_callback",
            pkce_verifier: None,
            nonce: None,
            query: &query,
        })
        .await
        .unwrap();
    assert_eq!(tokens.access_token, "oidc-access-token");
    assert!(tokens.expires_at.is_some());
    assert!(!tokens.is_expired());
    assert!(tokens.can_refresh());

    let refreshed = provider.refresh_token("refresh-token").await.unwrap();
    assert_eq!(refreshed.access_token, "refreshed-token");
}

#[tokio::test]
async fn cloudflare_verifies_the_access_jwt_and_maps_sub() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}");
    let key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
    let key_set = jwks(&key, "cf-key");
    let app = Router::new().route(
        "/cdn-cgi/access/certs",
        get(move || {
            let key_set = key_set.clone();
            async move { Json(key_set) }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let provider = factory::builtin_registry()
        .create_and_init(
            "CloudflareAccess",
            json!({"team_domain": base, "policy_aud": "aud-tag"}),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();

    let jwt = sign_jwt(
        &key,
        "cf-key",
        json!({
            "iss": base,
            "aud": "aud-tag",
            "sub": "cf-user",
            "email": "user@example.com",
            "exp": chrono::Utc::now().timestamp() + 600,
        }),
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        "Cf-Access-Jwt-Assertion",
        HeaderValue::from_str(&jwt).unwrap(),
    );
    let query = CallbackQuery::new();
    let user = provider
        .callback(callback(
            &request.state,
            Some(&request.state),
            None,
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap();
    assert_eq!(user.subject, "cf-user");
    assert_eq!(user.email.as_deref(), Some("user@example.com"));

    let wrong = sign_jwt(
        &key,
        "cf-key",
        json!({
            "iss": base,
            "aud": "wrong-audience",
            "sub": "cf-user",
            "exp": chrono::Utc::now().timestamp() + 600,
        }),
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        "Cf-Access-Jwt-Assertion",
        HeaderValue::from_str(&wrong).unwrap(),
    );
    let request = provider.authorization_url(redirect).await.unwrap();
    let error = provider
        .callback(callback(
            &request.state,
            Some(&request.state),
            None,
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, OAuthError::InvalidToken(_)));
}

#[tokio::test]
async fn manager_seeds_provider_rows_and_loads_the_configured_provider() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    let manager = OAuthManager::new();
    manager.initialize_from_db(&db).await.unwrap();
    assert_eq!(manager.current_name(), Some("github"));
    assert_eq!(db.oauth().count().unwrap(), 4);
    assert!(db.oauth().get("generic").unwrap().is_some());
}

#[tokio::test]
async fn account_linker_creates_and_binds_without_overwriting_a_password() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    let linker = AccountLinker::new(&db);
    let identity = SsoIdentity::new("github", "12345");

    let created = linker.find_or_create(&identity, Some("octocat")).unwrap();
    assert!(created.created);
    assert_eq!(created.user.sso_type, "github");
    assert_eq!(created.user.sso_id, "github_12345");
    assert_eq!(created.session_user().username, "octocat");

    let local = db.accounts().create("admin", "local-password").unwrap();
    let local_identity = SsoIdentity::new("github", "999");
    let bound = linker.bind(&local.uuid, &local_identity).unwrap();
    assert_eq!(bound.sso_id, "github_999");
    let stored = db.accounts().require_by_uuid(&local.uuid).unwrap();
    assert!(stored.check_password("local-password"));
    assert_eq!(stored.passwd, local.passwd);

    let other = db.accounts().create("other", "other-password").unwrap();
    assert!(matches!(
        linker.bind(&other.uuid, &local_identity),
        Err(OAuthError::AlreadyBound)
    ));
}

#[tokio::test]
async fn token_error_body_does_not_leak_json_tokens() {
    let app = Router::new().route(
        "/token",
        post(|| async {
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "invalid_grant",
                    "error_description": "bad authorization code",
                    "access_token": "leaked-access",
                    "refresh_token": "leaked-refresh",
                    "client_secret": "leaked-secret"
                })),
            )
        }),
    );
    let base = spawn(app).await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "auth_url": "https://idp.example/authorize",
                "token_url": format!("{base}/token"),
                "user_info_url": "https://idp.example/userinfo",
                "scope": "openid",
                "user_id_field": "sub",
            }),
        )
        .await
        .unwrap();
    let query = CallbackQuery::new();
    let error = provider
        .exchange_code(ExchangeRequest {
            code: "code",
            redirect_uri: "https://monitor.example/api/oauth_callback",
            pkce_verifier: None,
            nonce: None,
            query: &query,
        })
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("invalid_grant"));
    assert!(!message.contains("leaked-access"));
    assert!(!message.contains("leaked-refresh"));
    assert!(!message.contains("leaked-secret"));
}

#[tokio::test]
async fn generic_ignores_id_token_when_oidc_is_not_configured() {
    let app = Router::new()
        .route(
            "/token",
            post(|| async {
                Json(json!({
                    "access_token": "plain-token",
                    "token_type": "Bearer",
                    "id_token": "not-a-jwt"
                }))
            }),
        )
        .route(
            "/userinfo",
            get(|| async { Json(json!({"sub": "plain-user", "name": "Plain"})) }),
        );
    let base = spawn(app).await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "auth_url": "https://idp.example/authorize",
                "token_url": format!("{base}/token"),
                "user_info_url": format!("{base}/userinfo"),
                "scope": "openid",
                "user_id_field": "sub",
            }),
        )
        .await
        .unwrap();
    let query = CallbackQuery::new();
    let tokens = provider
        .exchange_code(ExchangeRequest {
            code: "code",
            redirect_uri: "https://monitor.example/api/oauth_callback",
            pkce_verifier: None,
            nonce: None,
            query: &query,
        })
        .await
        .unwrap();
    assert!(tokens.id_token.is_none());
    let info = provider.fetch_user_info(&tokens).await.unwrap();
    assert_eq!(info.subject, "plain-user");
}

#[tokio::test]
async fn generic_requires_an_id_token_when_configured() {
    let app = Router::new().route(
        "/token",
        post(|| async { Json(json!({"access_token": "plain-token", "token_type": "Bearer"})) }),
    );
    let base = spawn(app).await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "auth_url": "https://idp.example/authorize",
                "token_url": format!("{base}/token"),
                "user_info_url": "https://idp.example/userinfo",
                "jwks_url": "https://idp.example/jwks",
                "scope": "openid",
                "user_id_field": "sub",
                "id_token_required": true,
            }),
        )
        .await
        .unwrap();
    let query = CallbackQuery::new();
    let error = provider
        .exchange_code(ExchangeRequest {
            code: "code",
            redirect_uri: "https://monitor.example/api/oauth_callback",
            pkce_verifier: None,
            nonce: None,
            query: &query,
        })
        .await
        .unwrap_err();
    assert!(matches!(error, OAuthError::InvalidToken(_)));
}

#[tokio::test]
async fn generic_nonce_mismatch_is_rejected() {
    let (base, _key) = oidc_server("client-id").await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "issuer": base,
                "user_id_field": "sub",
                "id_token_required": true,
                "use_nonce": true,
            }),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    assert!(request.url.contains("&nonce="));
    let query = CallbackQuery::new();
    let headers = HeaderMap::new();
    let error = provider
        .callback(callback(
            &request.state,
            Some(&request.state),
            Some("code"),
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, OAuthError::InvalidToken(_)));
}

#[tokio::test]
async fn generic_skips_discovery_when_every_endpoint_is_explicit() {
    let discovery_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&discovery_calls);
    let app = Router::new().route(
        "/.well-known/openid-configuration",
        get(move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Json(json!({"issuer": "https://unexpected.example"}))
            }
        }),
    );
    let base = spawn(app).await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "issuer": base,
                "auth_url": format!("{base}/authorize"),
                "token_url": format!("{base}/token"),
                "user_info_url": format!("{base}/userinfo"),
                "jwks_url": format!("{base}/jwks"),
                "scope": "openid",
                "user_id_field": "sub",
            }),
        )
        .await
        .unwrap();
    assert_eq!(discovery_calls.load(Ordering::SeqCst), 0);
    let request = provider
        .authorization_url("https://monitor.example/api/oauth_callback")
        .await
        .unwrap();
    assert!(request.url.starts_with(&format!("{base}/authorize?")));
}

#[tokio::test]
async fn qq_requires_social_uid_even_when_openid_is_present() {
    let app = Router::new().route(
        "/connect.php",
        get(|Query(query): Query<HashMap<String, String>>| async move {
            if query.get("act").map(String::as_str) == Some("login") {
                Json(json!({"code": 0, "msg": "ok", "url": "https://login.example/auth"}))
            } else {
                Json(json!({
                    "code": 0,
                    "msg": "ok",
                    "social_uid": "",
                    "openid": "openid-only",
                    "unionid": "union-only"
                }))
            }
        }),
    );
    let base = spawn(app).await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "qq",
            json!({
                "aggregation_url": base,
                "app_id": "app-id",
                "app_key": "app-key",
                "login_type": "qq",
            }),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    let mut query = CallbackQuery::new();
    query.insert("code".to_owned(), "code".to_owned());
    let headers = HeaderMap::new();
    let error = provider
        .callback(callback(
            &request.state,
            None,
            Some("code"),
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("empty social_uid"));
}

#[tokio::test]
async fn cloudflare_rejects_a_callback_without_the_access_header() {
    let provider = factory::builtin_registry()
        .create_and_init(
            "CloudflareAccess",
            json!({"team_domain": "https://team.example", "policy_aud": "aud"}),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    let query = CallbackQuery::new();
    let headers = HeaderMap::new();
    let error = provider
        .callback(callback(
            &request.state,
            Some(&request.state),
            None,
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no Cloudflare Access JWT"));
}

#[tokio::test]
async fn jwks_is_not_refreshed_for_invalid_signatures_but_is_for_unknown_kids() {
    let jwks_key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
    let signing_key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
    let key_set = jwks(&jwks_key, "test-key");
    let fetches = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fetches);
    let app = Router::new().route(
        "/jwks",
        get(move || {
            let key_set = key_set.clone();
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Json(key_set)
            }
        }),
    );
    let base = spawn(app).await;
    let verifier = komari_oauth::jwt::JwtVerifier::new(
        reqwest::Client::new(),
        format!("{base}/jwks"),
        Some(base.clone()),
        Some("aud".to_owned()),
    );
    let claims = || {
        json!({
            "iss": base,
            "aud": "aud",
            "sub": "user",
            "exp": chrono::Utc::now().timestamp() + 600
        })
    };

    let wrong_signature = sign_jwt(&signing_key, "test-key", claims());
    assert!(verifier.verify(&wrong_signature).await.is_err());
    assert!(verifier.verify(&wrong_signature).await.is_err());
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    let unknown_kid = sign_jwt(&signing_key, "rotated-key", claims());
    assert!(verifier.verify(&unknown_kid).await.is_err());
    assert_eq!(fetches.load(Ordering::SeqCst), 2);
    assert!(verifier.verify(&unknown_kid).await.is_err());
    assert_eq!(fetches.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn generic_requires_user_id_field_like_go() {
    let error = match factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "auth_url": "https://idp.example/authorize",
                "token_url": "https://idp.example/token",
                "user_info_url": "https://idp.example/userinfo",
                "scope": "openid",
            }),
        )
        .await
    {
        Ok(_) => panic!("generic provider without user_id_field should fail"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("user_id_field"));
}

#[tokio::test]
async fn qq_default_config_matches_the_go_zero_value() {
    assert_eq!(factory::default_config("qq")["aggregation_url"], "");
    let qq_fields = factory::builtin_registry().config_fields("qq").unwrap();
    assert_eq!(qq_fields[0].default, Some("https://login.qjqq.cn"));
}

#[tokio::test]
async fn error_bodies_without_safe_fields_are_not_echoed() {
    let app = Router::new().route(
        "/token",
        post(|| async {
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"token": "leaky-value"})),
            )
        }),
    );
    let base = spawn(app).await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "auth_url": "https://idp.example/authorize",
                "token_url": format!("{base}/token"),
                "user_info_url": "https://idp.example/userinfo",
                "scope": "openid",
                "user_id_field": "sub",
            }),
        )
        .await
        .unwrap();
    let query = CallbackQuery::new();
    let error = provider
        .exchange_code(ExchangeRequest {
            code: "code",
            redirect_uri: "https://monitor.example/api/oauth_callback",
            pkce_verifier: None,
            nonce: None,
            query: &query,
        })
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("leaky-value"));
    assert!(error.to_string().contains("no error details"));

    let app = Router::new().route(
        "/token",
        post(|| async { (axum::http::StatusCode::BAD_REQUEST, "leaky-value") }),
    );
    let base = spawn(app).await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "auth_url": "https://idp.example/authorize",
                "token_url": format!("{base}/token"),
                "user_info_url": "https://idp.example/userinfo",
                "scope": "openid",
                "user_id_field": "sub",
            }),
        )
        .await
        .unwrap();
    let error = provider
        .exchange_code(ExchangeRequest {
            code: "code",
            redirect_uri: "https://monitor.example/api/oauth_callback",
            pkce_verifier: None,
            nonce: None,
            query: &query,
        })
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("leaky-value"));
    assert!(error.to_string().contains("non-JSON error body"));

    let app = Router::new().route(
        "/token",
        post(|| async { (axum::http::StatusCode::OK, "leaky-value") }),
    );
    let base = spawn(app).await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "auth_url": "https://idp.example/authorize",
                "token_url": format!("{base}/token"),
                "user_info_url": "https://idp.example/userinfo",
                "scope": "openid",
                "user_id_field": "sub",
            }),
        )
        .await
        .unwrap();
    let error = provider
        .exchange_code(ExchangeRequest {
            code: "code",
            redirect_uri: "https://monitor.example/api/oauth_callback",
            pkce_verifier: None,
            nonce: None,
            query: &query,
        })
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("leaky-value"));
    assert!(error.to_string().contains("invalid JSON response"));
}

#[tokio::test]
async fn non_object_json_error_bodies_are_not_echoed() {
    for body in [json!(["leaky-value"]), json!("leaky-value")] {
        let app = Router::new().route(
            "/token",
            post(move || {
                let body = body.clone();
                async move { (axum::http::StatusCode::BAD_REQUEST, Json(body)) }
            }),
        );
        let base = spawn(app).await;
        let provider = factory::builtin_registry()
            .create_and_init(
                "generic",
                json!({
                    "client_id": "client-id",
                    "client_secret": "client-secret",
                    "auth_url": "https://idp.example/authorize",
                    "token_url": format!("{base}/token"),
                    "user_info_url": "https://idp.example/userinfo",
                    "scope": "openid",
                    "user_id_field": "sub",
                }),
            )
            .await
            .unwrap();
        let query = CallbackQuery::new();
        let error = provider
            .exchange_code(ExchangeRequest {
                code: "code",
                redirect_uri: "https://monitor.example/api/oauth_callback",
                pkce_verifier: None,
                nonce: None,
                query: &query,
            })
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("leaky-value"));
        assert!(
            error
                .to_string()
                .contains("error body is not a JSON object")
        );
    }
}

#[tokio::test]
async fn generic_oidc_uses_sub_even_when_user_id_field_is_email() {
    let (base, _key) = oidc_server("client-id").await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "issuer": base,
                "user_id_field": "email",
                "id_token_required": true,
            }),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    let query = CallbackQuery::new();
    let headers = HeaderMap::new();
    let user = provider
        .callback(callback(
            &request.state,
            Some(&request.state),
            Some("code"),
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap();
    assert_eq!(user.subject, "oidc-user");
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
}

#[tokio::test]
async fn generic_oidc_without_userinfo_uses_verified_id_token_claims() {
    let (base, _key) = oidc_server("client-id").await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "issuer": base,
                "auth_url": format!("{base}/authorize"),
                "token_url": format!("{base}/token"),
                "jwks_url": format!("{base}/jwks"),
                "user_id_field": "sub",
                "id_token_required": true,
            }),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    let query = CallbackQuery::new();
    let headers = HeaderMap::new();
    let user = provider
        .callback(callback(
            &request.state,
            Some(&request.state),
            Some("code"),
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap();
    assert_eq!(user.subject, "oidc-user");
}

#[tokio::test]
async fn redirect_base_is_enforced_by_every_provider() {
    let qq_app = Router::new().route(
        "/connect.php",
        get(|| async {
            Json(json!({"code": 0, "msg": "ok", "url": "https://login.example/auth"}))
        }),
    );
    let qq_base = spawn(qq_app).await;
    let redirect_base = "https://monitor.example/api/oauth_callback";
    let providers = vec![
        (
            "generic",
            json!({
                "client_id": "client-id",
                "client_secret": "client-secret",
                "auth_url": "https://idp.example/authorize",
                "token_url": "https://idp.example/token",
                "user_info_url": "https://idp.example/userinfo",
                "scope": "openid",
                "user_id_field": "sub",
                "redirect_base": redirect_base,
            }),
        ),
        (
            "github",
            json!({
                "client_id": "gh-client",
                "client_secret": "gh-secret",
                "redirect_base": redirect_base,
            }),
        ),
        (
            "qq",
            json!({
                "aggregation_url": qq_base,
                "app_id": "app-id",
                "app_key": "app-key",
                "login_type": "qq",
                "redirect_base": redirect_base,
            }),
        ),
        (
            "CloudflareAccess",
            json!({
                "team_domain": "https://team.example",
                "policy_aud": "aud",
                "redirect_base": redirect_base,
            }),
        ),
    ];

    for (name, config) in providers {
        let provider = factory::builtin_registry()
            .create_and_init(name, config)
            .await
            .unwrap();
        assert!(
            matches!(
                provider
                    .authorization_url("https://evil.example/api/oauth_callback")
                    .await,
                Err(OAuthError::InvalidRedirectUri(_))
            ),
            "{name} accepted a foreign redirect host"
        );
        assert!(
            provider.authorization_url(redirect_base).await.is_ok(),
            "{name} rejected the configured redirect base"
        );
    }
}

#[tokio::test]
async fn qq_default_config_is_loadable_and_manager_can_select_qq() {
    let default = factory::default_config("qq");
    let provider = factory::load_provider("qq", &default.to_string())
        .await
        .unwrap();
    let error = provider
        .authorization_url("https://monitor.example/api/oauth_callback")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("aggregation_url"));

    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    db.config()
        .set("o_auth_provider", &"qq".to_owned())
        .unwrap();
    let manager = OAuthManager::new();
    manager.initialize_from_db(&db).await.unwrap();
    assert_eq!(manager.current_name(), Some("qq"));
}

#[tokio::test]
async fn jwks_is_not_refreshed_for_expired_or_mismatched_claims() {
    let key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
    let key_set = jwks(&key, "test-key");
    let fetches = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fetches);
    let app = Router::new().route(
        "/jwks",
        get(move || {
            let key_set = key_set.clone();
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Json(key_set)
            }
        }),
    );
    let base = spawn(app).await;
    let verifier = komari_oauth::jwt::JwtVerifier::new(
        reqwest::Client::new(),
        format!("{base}/jwks"),
        Some(base.clone()),
        Some("aud".to_owned()),
    );
    let now = chrono::Utc::now().timestamp();

    let wrong_audience = sign_jwt(
        &key,
        "test-key",
        json!({"iss": base, "aud": "other", "sub": "user", "exp": now + 600}),
    );
    assert!(verifier.verify(&wrong_audience).await.is_err());
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    let wrong_issuer = sign_jwt(
        &key,
        "test-key",
        json!({"iss": "https://other.example", "aud": "aud", "sub": "user", "exp": now + 600}),
    );
    assert!(verifier.verify(&wrong_issuer).await.is_err());
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    let expired = sign_jwt(
        &key,
        "test-key",
        json!({"iss": base, "aud": "aud", "sub": "user", "exp": now - 600}),
    );
    assert!(verifier.verify(&expired).await.is_err());
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cloudflare_query_token_requires_the_explicit_switch() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}");
    let key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
    let key_set = jwks(&key, "cf-key");
    let app = Router::new().route(
        "/cdn-cgi/access/certs",
        get(move || {
            let key_set = key_set.clone();
            async move { Json(key_set) }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let jwt = sign_jwt(
        &key,
        "cf-key",
        json!({
            "iss": base,
            "aud": "aud-tag",
            "sub": "cf-user",
            "exp": chrono::Utc::now().timestamp() + 600,
        }),
    );
    let query = CallbackQuery::new();

    let default_provider = factory::builtin_registry()
        .create_and_init(
            "CloudflareAccess",
            json!({"team_domain": base, "policy_aud": "aud-tag"}),
        )
        .await
        .unwrap();
    assert!(matches!(
        default_provider
            .exchange_code(ExchangeRequest {
                code: &jwt,
                redirect_uri: "https://monitor.example/api/oauth_callback",
                pkce_verifier: None,
                nonce: None,
                query: &query,
            })
            .await,
        Err(OAuthError::Unsupported(_))
    ));

    let provider = factory::builtin_registry()
        .create_and_init(
            "CloudflareAccess",
            json!({
                "team_domain": base,
                "policy_aud": "aud-tag",
                "allow_query_token": true
            }),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    let mut query = CallbackQuery::new();
    query.insert("access_token".to_owned(), jwt.clone());
    let headers = HeaderMap::new();
    let user = provider
        .callback(callback(
            &request.state,
            Some(&request.state),
            None,
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap();
    assert_eq!(user.subject, "cf-user");

    let tokens = provider
        .exchange_code(ExchangeRequest {
            code: &jwt,
            redirect_uri: redirect,
            pkce_verifier: None,
            nonce: None,
            query: &query,
        })
        .await
        .unwrap();
    assert_eq!(tokens.access_token, jwt);
}

#[tokio::test]
async fn qq_rejects_a_non_string_social_uid() {
    let app = Router::new().route(
        "/connect.php",
        get(|Query(query): Query<HashMap<String, String>>| async move {
            if query.get("act").map(String::as_str) == Some("login") {
                Json(json!({"code": 0, "msg": "ok", "url": "https://login.example/auth"}))
            } else {
                Json(json!({
                    "code": 0,
                    "msg": "ok",
                    "social_uid": 12345,
                    "openid": "openid-only"
                }))
            }
        }),
    );
    let base = spawn(app).await;
    let provider = factory::builtin_registry()
        .create_and_init(
            "qq",
            json!({
                "aggregation_url": base,
                "app_id": "app-id",
                "app_key": "app-key",
                "login_type": "qq",
            }),
        )
        .await
        .unwrap();
    let redirect = "https://monitor.example/api/oauth_callback";
    let request = provider.authorization_url(redirect).await.unwrap();
    let query = CallbackQuery::new();
    let headers = HeaderMap::new();
    let error = provider
        .callback(callback(
            &request.state,
            None,
            Some("code"),
            redirect,
            &query,
            &headers,
        ))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("empty social_uid"));
}
