# komari-oauth

OAuth2 / OpenID Connect providers and external-account binding for the Rust
Komari backend.

This crate is the port of the Go backend's `utils/oauth/**` packages plus the
account-linking logic used by `api/public/oauth.go`. It is consumed by the
login and binding endpoints in later tasks; it does not expose HTTP routes
itself.

## Providers

| Name | Go source | Behaviour |
| --- | --- | --- |
| `generic` | `utils/oauth/generic` | Generic OAuth2 user-info flow, plus optional OIDC discovery, JWKS and `id_token` validation |
| `github` | `utils/oauth/github` | GitHub authorize/token/user endpoints; falls back to `/user/emails` when the profile has no public email |
| `qq` | `utils/oauth/qq` | QQ aggregation login (`login.qjqq.cn` by default), `social_uid`/`openid`/`unionid` mapping and aggregation error codes |
| `CloudflareAccess` | `utils/oauth/cloudflare` | Cloudflare Access JWT verification against the team's JWKS with `policy_aud` as audience |

The default endpoints, scopes and request parameters match the Go
implementations:

* `generic` authorization URL parameter order is
  `client_id`, `state`, `scope`, `redirect_uri`, `response_type=code`.
* `github` uses `scope=user:email` and deliberately omits `redirect_uri`.
* `qq` calls `{aggregation_url}/connect.php?act=login...` while building the
  URL and `act=callback` during the callback.
* `CloudflareAccess` appends an 8-character `state` to the callback URI.

Optional fields that are not part of the Go structs are available for GitHub
Enterprise and tests (for example `authorize_url`, `token_url`,
`user_api_url`, `email_api_url` on GitHub, and `issuer`, `jwks_url`,
`id_token_required`, `use_pkce`, `use_nonce` on `generic`). They are not
listed in the factory metadata shown to the admin UI.

## Quick start

```rust
use komari_oauth::{factory, CallbackRequest, CallbackQuery};

# async fn example() -> komari_oauth::Result<()> {
let provider = factory::load_provider(
    "github",
    r#"{"client_id":"id","client_secret":"secret"}"#,
).await?;

let request = provider
    .authorization_url("https://monitor.example/api/oauth_callback")
    .await?;
// Store request.state in the `oauth_state` cookie, then redirect the browser.
println!("{}", request.url);
# Ok(())
# }
```

On the callback:

```rust
# use komari_oauth::{CallbackRequest, CallbackQuery};
# use reqwest::header::HeaderMap;
# async fn callback(provider: std::sync::Arc<dyn komari_oauth::OAuthProvider>, state: &str, code: &str) -> komari_oauth::Result<()> {
let mut query = CallbackQuery::new();
query.insert("code".to_owned(), code.to_owned());
let headers = HeaderMap::new();
let user = provider
    .callback(CallbackRequest {
        state,                       // cookie value
        query_state: query.get("state").map(String::as_str),
        code: Some(code),
        redirect_uri: "https://monitor.example/api/oauth_callback",
        query: &query,
        headers: &headers,           // Cloudflare Access reads its JWT here
    })
    .await?;
println!("{} {}", user.provider, user.subject);
# Ok(())
# }
```

`state` values are random, bound to the redirect URI and single-use. They
expire after five minutes. When `use_pkce` is enabled on `generic`, the
verifier is kept with the state entry and sent as `code_verifier` during the
token exchange.

## Account binding

`AccountLinker` uses the `komari-db` accounts repository and stores the
Go-compatible identity `"{provider}_{subject}"` in `users.sso_id`, with the
provider name in `users.sso_type`.

```rust
use komari_oauth::{AccountLinker, SsoIdentity};

# fn example(db: &komari_db::Db) -> komari_oauth::Result<()> {
let linker = AccountLinker::new(db);
let identity = SsoIdentity::new("github", "12345");

// Find or create a local account. New accounts get a random password.
let account = linker.find_or_create(&identity, Some("octocat"))?;
println!("{}", account.session_user().uuid);

// Bind an existing local account without touching its password.
linker.bind(&account.user.uuid, &identity)?;
# Ok(())
# }
```

Rules:

* `bind` refuses an identity that already belongs to another account
  (`AlreadyBound`) and refuses to replace a different binding
  (`UserAlreadyBound`). `rebind` replaces only the target account's own
  binding.
* Existing password hashes are never read or written by the linker.
* `find_or_create_from_user` uses the provider username or email local part as
  the preferred local username and adds a numeric suffix on collision.
* `OAuthAccount::session_user()` returns the fields needed to create a session
  (`uuid`, `username`, `sso_type`, `sso_id`).

## Manager and database configuration

`OAuthManager::initialize_from_db` mirrors Go's `oauth.Initialize`:

1. insert a default `oidc_providers` row for every built-in provider that is
   missing;
2. read `o_auth_provider` from the key/value `configs` table;
3. load that provider's `addition` JSON, falling back to `github` with `{}`.

`OAuthManager::current()` is the equivalent of `oauth.CurrentProvider()`.

## Security notes

* `client_secret`, `app_key`, `access_token`, `refresh_token` and `id_token`
  values are replaced with `********` before any transport or response error is
  returned. Provider configuration `Debug` implementations also mask secrets.
* `TokenSet` has a custom `Debug` implementation that never prints tokens.
* Redirect URIs must be absolute `http`/`https` URLs with a host, without a
  fragment or embedded credentials.
* OIDC `id_token` signatures are verified with RS256/RS384/RS512 using `ring`.
  `iss`, `aud`, `exp` and `nbf` are checked; a JWKS refresh is attempted once.
* QQ intentionally skips the cookie-vs-query state comparison because the
  aggregation platform does not preserve it, matching Go's
  `providersSkipStateCheck`.

## Tests

```sh
cargo test -p komari-oauth
cargo clippy -p komari-oauth --all-targets -- -D warnings
```

The test suite covers factory metadata, authorization URL parameters for all
four providers, mocked token/user-info exchanges, GitHub email fallback, QQ
error codes, state mismatch and redirect mismatch, provider error callbacks,
OIDC discovery/JWKS/id-token validation (including a wrong audience), Cloudflare
JWT verification, PKCE, token refresh/expiry, secret redaction, and
account creation/binding/duplicate-binding behaviour.

## Known limitations

* Only RS* JWTs are verified. ES256/EdDSA providers are not supported yet.
* The generic provider does not implement the full OpenID Connect RP library
  surface (no userinfo-only OIDC session management, no token introspection or
  revocation).
* QQ and Cloudflare Access do not expose a refresh-token flow; calling
  `refresh_token` returns `OAuthError::Unsupported`.
* `cargo test` uses local `axum` mock servers; no external network access is
  required.
