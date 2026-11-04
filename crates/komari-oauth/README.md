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
| `generic` | `utils/oauth/generic` | Generic OAuth2 user-info flow; optional OIDC discovery/JWKS/`id_token` validation when configured, otherwise `id_token` is ignored like Go |
| `github` | `utils/oauth/github` | GitHub authorize/token/user endpoints; falls back to `/user/emails` when the profile has no public email |
| `qq` | `utils/oauth/qq` | QQ aggregation login, strict `social_uid` subject, `openid`/`unionid` retained as metadata, aggregation error codes |
| `CloudflareAccess` | `utils/oauth/cloudflare` | Cloudflare Access JWT from `Cf-Access-Jwt-Assertion`, verified against the team's JWKS with `policy_aud` as audience |

The default endpoints, scopes and request parameters match the Go
implementations:

* `generic` authorization URL parameter order is
  `client_id`, `state`, `scope`, `redirect_uri`, `response_type=code`.
* `github` uses `scope=user:email` and deliberately omits `redirect_uri`.
* `qq` calls `{aggregation_url}/connect.php?act=login...` while building the
  URL and `act=callback` during the callback.
* `CloudflareAccess` appends an 8-character `state` to the callback URI and
  reads the access JWT from `Cf-Access-Jwt-Assertion` by default.
* QQ stores `aggregation_url: ""` by default, matching Go's zero value. The
  factory metadata still suggests `https://login.qjqq.cn` from the Go struct
  tag.

Optional fields that are not part of the Go structs are available for GitHub
Enterprise and tests (for example `authorize_url`, `token_url`,
`user_api_url`, `email_api_url` on GitHub; `issuer`, `jwks_url`,
`id_token_required`, `use_pkce`, `use_nonce` on `generic`; and
`allow_query_token` on `CloudflareAccess`). They are not listed in the factory
metadata shown to the admin UI.

Every provider also accepts an optional `redirect_base` (the public callback
URL). When set, `authorization_url` rejects a callback whose scheme, host,
port or path differs, so an attacker-controlled `Host` header cannot turn into
an open redirect. API layers should set it to the same value used to build the
callback URL.

`generic` requires a non-empty `user_id_field` (Go does too). For pure OIDC
providers use `"sub"`. When `issuer`, `auth_url`, `token_url` and `jwks_url`
are configured, discovery is skipped; `user_info_url` is optional when
`id_token` validation is enabled because verified claims are used instead.
When OIDC validation is active, the account subject is always the verified
`sub`, even if `user_id_field` is set to something else.

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

// Go-compatible login: fail with AccountNotBound (map to HTTP 401) when the
// external identity has not been bound by an administrator. The returned
// SessionUser excludes the password hash and is safe to serialize.
let session = linker.login(&identity)?;
println!("{}", session.uuid);

// Explicit opt-in self-service registration (not used by the Go login flow).
let account = linker.find_or_create(&identity, Some("octocat"))?;
println!("{}", account.session_user().uuid);

// Bind an existing local account without touching its password.
linker.bind(&account.user.uuid, &identity)?;
# Ok(())
# }
```

Rules:

* `login` is the Go-compatible path: it returns a serializable
  `SessionUser` (uuid/username/sso_type/sso_id, no password hash). An unknown
  identity returns `OAuthError::AccountNotBound` and must not create a user.
* `find_or_create` / `find_or_create_from_user` are explicit opt-in
  registration helpers. New accounts get a random local password.
* `find`, `bind`, `rebind` and `unbind` return a serializable `SessionUser`
  and never expose the password hash or TOTP secret. Do not serialize the
  `komari-db` `User` model directly in API responses.
* `bind` refuses an identity that already belongs to another account
  (`AlreadyBound`) and refuses to replace a different binding
  (`UserAlreadyBound`). `rebind` replaces only the target account's own
  binding.
* Existing password hashes are never read or written by the linker.
* `find_or_create_from_user` uses the provider username or email local part as
  the preferred local username and adds a numeric suffix on collision.
* `OAuthAccount::session_user()` returns the fields needed to create a session
  (`uuid`, `username`, `sso_type`, `sso_id`). Its `Debug` output omits the
  password hash.

## Manager and database configuration

`OAuthManager::initialize_from_db` mirrors Go's `oauth.Initialize`:

1. insert a default `oidc_providers` row for every built-in provider that is
   missing;
2. read `o_auth_provider` from the key/value `configs` table;
3. load that provider's `addition` JSON, falling back to `github` with `{}`.

`OAuthManager::current()` is the equivalent of `oauth.CurrentProvider()`.

## Security notes

* `client_secret`, `app_key`, `access_token`, `refresh_token`, `id_token`,
  `code`, `state`, `passwd`, `two_factor` and TOTP values are replaced with
  `********` before any transport or response error is returned. Redaction
  understands query/form values, JSON object keys and nested JSON, and
  normalizes separators and casing (`access_token`, `accessToken`,
  `access-token` and `access.token` are equivalent). Non-2xx JSON bodies
  retain only `error`/`error_description`/`error_uri`/`message`/`msg`;
  unknown fields, non-object JSON and non-JSON bodies are never echoed.
* Provider raw payloads (`UserInfo::raw`, `OAuthUser::raw`, `TokenSet::raw`)
  are not serialized, are omitted from `Debug`, and are recursively stripped
  of token/secret keys before storage.
* `TokenSet` never serializes or `Debug`-prints access/refresh/id tokens.
  `AuthorizationRequest`, `Pkce`, `StateEntry` and `StateStore` hide state and
  PKCE verifier values.
* Redirect URIs must be absolute `https` URLs, or loopback `http`, with a host,
  without a fragment or embedded credentials. Set the provider's
  `redirect_base` (or call `RedirectPolicy` /
  `validate_redirect_uri_for_base` in the API layer) to reject
  `evil.example`; shape validation alone cannot know the real callback host.
* OIDC `id_token` signatures are verified with the full go-oidc/go-jose
  asymmetric set: RS256/384/512, PS256/384/512, ES256/384/512 and EdDSA
  (Ed25519), using RSA, EC (P-256/P-384/P-521) and OKP JWKS keys. `alg=none`,
  unknown algorithms and algorithm/key-type confusion are rejected. `iss`,
  `aud`, `exp` and `nbf` are checked. JWKS is refreshed only for an unknown
  `kid`, at most once per 30 seconds; invalid signatures or claim mismatches
  never trigger a refresh.
* Cloudflare Access accepts only the `Cf-Access-Jwt-Assertion` header by
  default. Set `allow_query_token: true` explicitly for non-browser clients.
  `exchange_code` also enforces the switch and returns
  `OAuthError::Unsupported` otherwise.
* QQ intentionally skips the cookie-vs-query state comparison because the
  aggregation platform does not preserve it, matching Go's
  `providersSkipStateCheck`. Only a string `social_uid` is accepted as the
  subject; `openid`/`unionid` alone or a numeric `social_uid` are an error.
* A QQ provider with the Go zero-value configuration is loadable (matching
  Go); `aggregation_url` is reported when `authorization_url` is requested.

## Tests

```sh
cargo test -p komari-oauth
cargo clippy -p komari-oauth --all-targets -- -D warnings
```

The test suite covers factory metadata and defaults, authorization URL
parameters for all four providers, mocked token/user-info exchanges, GitHub
email fallback, QQ error codes and empty `social_uid`, state mismatch and
redirect mismatch, provider error callbacks, OIDC discovery/JWKS/id-token
validation (wrong audience, missing `id_token`, nonce mismatch, discovery
skip, OIDC subject override, no-userinfo id_token flow), every supported JWT
algorithm, `alg=none`, unknown algorithms, algorithm/key-type confusion, EC
JWKS parsing and rotation, Cloudflare JWT verification, missing-header
rejection and query-token switch, PKCE, token refresh/expiry,
JSON/query/form secret redaction (including camelCase, kebab-case, `code`,
`state`, `passwd` and `two_factor`), non-object JSON error bodies, JWKS refresh
amplification and claim-error no-refresh, redirect-base rejection for all four
providers, QQ default-config loadability, and account
creation/binding/duplicate-binding/legacy-row/`rebind`/`AccountNotBound`/
`SessionUser` serialization behaviour.

## Known limitations

* JWKS keys must publish `n`/`e`, `crv`/`x`/`y` or `crv`/`x`; `x5c`-only keys
  are not parsed.
* All go-oidc algorithms are accepted. There is no per-provider signing
  algorithm allowlist yet; pinning would require a configuration field.
* `generic` still requires `user_id_field`; pure OIDC users must set `"sub"`.
* The generic provider does not implement the full OpenID Connect RP library
  surface (no userinfo-only OIDC session management, no token introspection or
  revocation).
* QQ and Cloudflare Access do not expose a refresh-token flow; calling
  `refresh_token` returns `OAuthError::Unsupported`.
* Cloudflare Access query-token support is an explicit extension
  (`allow_query_token`), not Go behaviour.
* `cargo test` uses local `axum` mock servers; no external network access is
  required.
