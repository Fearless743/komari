# komari-web

Static asset serving for Komari: the embedded default theme, installed themes,
the SPA fallback and the favicon route. This crate is consumed by the HTTP
server crate (B06/B09); it does not define any business API.

## Build-time theme requirement

The default theme is embedded with [`rust-embed`]. Before building, the
`komari-web` frontend must produce its Vite output. The directory that is
embedded must look like this:

```text
<theme>/
  komari-theme.json
  preview.png          # or the historical perview.png spelling
  dist/
    index.html
    assets/...
```

A real theme can be produced with:

```sh
cd komari-web
npm run build
```

Then point the backend build at that checkout:

```sh
KOMARI_DEFAULT_THEME_DIR=/path/to/komari-web cargo build -p komari-web
```

If `KOMARI_DEFAULT_THEME_DIR` is unset, the placeholder committed under
`crates/komari-web/defaultTheme` is embedded so a fresh checkout still builds.
The build script prints a warning in that case. When the variable is set but the
directory is incomplete, the build fails with the missing path and the expected
layout (it never panics silently).

The placeholder is **not** a usable UI. Release builds must set
`KOMARI_DEFAULT_THEME_DIR`.

## Serving behaviour

| Request | Behaviour |
| --- | --- |
| `/favicon.ico` | `data/favicon.ico`, then the active theme's `dist/favicon.ico`, then the embedded default, else 404 |
| `/themes/{id}/{path}` | `data/theme/{id}/{path}` first, then the embedded default theme |
| unknown `GET`/`HEAD` path | active theme's `dist{path}`, otherwise `dist/index.html` |
| `/admin*` | default theme's `index.html`, no site replacements |
| `/api`, `/api/*` | always 404, never the SPA (intentional divergence, see below) |
| other methods on static/unknown paths | 404 |

Theme selection uses the configured theme short name. A theme is considered
available when `data/theme/<theme>/dist/index.html` is readable; a valid
`komari-theme.json` is only required for listings, previews and theme settings.
This matches Go, which serves any theme file that exists. If the configured
theme has no readable index, `default` is used. Files missing from an installed
theme fall back to the embedded default on a per-file basis, matching the Go
`getFileContent` helper.

### Differences from Go

* **`/api/*` is never the SPA.** Go's `NoRoute` currently serves `index.html` for
  an unknown `GET /api/...`; this crate returns 404 so API typos never look like
  a successful HTML response. This is the B14 requirement, not an accident.
* **Theme availability is based on `dist/index.html`, not on
  `komari-theme.json`.** Invalid or missing metadata no longer disables serving;
  it only makes the theme absent from `list_themes` and theme settings.
* **`list_themes` returns an empty list when `data/theme` does not exist.**
  This mirrors Go's early return; Go normally creates the directory in `init`,
  so the server layer should create `data/theme` during startup.
* **Static routes reject non-GET/HEAD with 404.** Gin's `NoRoute` also returns
  404 for these methods; this crate uses `any` plus an explicit method check to
  avoid Axum's default 405.
* **MIME types follow Go's built-ins** for the common cases: `.ico` is
  `image/vnd.microsoft.icon`, `.json` is `application/json` without a charset,
  and `.xml` is `text/xml; charset=utf-8`.

`index.html` is rewritten in a single pass:

* `<title>Komari Monitor</title>` → configured site name;
* `A simple server monitor tool.` → configured description;
* `</head>` → custom head HTML + `</head>`;
* `</body>` → custom body HTML + `</body>`.

Single-pass replacement matters: injected HTML is not scanned again.

## Headers

* `Content-Type` from the file extension, with `charset=utf-8` for text types.
* `Cache-Control`:
  * files below `assets/` → `public, max-age=31536000, immutable`;
  * `index.html` → `public, max-age=0, must-revalidate`;
  * other files → `public, max-age=300`.
* Strong `ETag` and `304 Not Modified` for `If-None-Match`.
* `Last-Modified` when the source exposes a timestamp.
* `HEAD` returns headers without a body.
* All static responses carry `Vary: Accept-Encoding`, including identity
  responses, so shared caches keep encodings separate.
* Optional pre-compressed `.br` / `.gz` siblings are served when the client
  accepts them. Negotiation honours `q` values, `q=0`, and `*`, and falls back
  to the next acceptable coding when the preferred file is missing. Disable
  with `WebConfig::precompressed = false`.

## Path safety

Theme ids and file paths are validated before touching the filesystem:

* `..` components, backslashes and NUL bytes are rejected;
* theme ids cannot contain separators;
* the canonical target must stay below the canonical theme root, so symlinks
  cannot escape `data/theme/<id>`;
* the SPA fallback rejects traversal-looking paths before serving
  `index.html`.

## Usage

```rust
use std::sync::Arc;
use komari_web::{SiteMeta, WebAssets, WebConfig};

let assets = Arc::new(WebAssets::new(WebConfig {
    data_dir: "data".into(),
    theme: "default".into(),
    ..WebConfig::default()
}));

// Update values when settings change.
assets.try_set_theme("my-theme").expect("installed theme");
assets.set_site_meta(SiteMeta {
    sitename: "My Monitor".into(),
    description: "status page".into(),
    custom_head: String::new(),
    custom_body: String::new(),
});

// Load these from the `tempory_share_token` /
// `tempory_share_token_expire_at` config rows whenever they change.
assets.set_temp_share_token("share-token", 1_900_000_000);

// Merge the static routes into the application router and use the fallback.
let app = komari_web::routes(assets.clone()).fallback({
    let assets = assets.clone();
    move |request| {
        let assets = assets.clone();
        async move { komari_web::serve_fallback(&assets, request).await }
    }
});
```

`komari_web::router(assets)` builds the same routes plus the fallback for tests
or standalone use. `routes` intentionally does not install a fallback, so the
host application keeps control of routing.

When the application already has its own state type, use the generic builders
so the static routes can be merged before the state is attached:

```rust
let app: axum::Router<AppState> = komari_web::routes_for::<AppState>(assets)
    .merge(api_routes);
```

`router_for::<AppState>` adds the SPA fallback as well.

## Theme metadata

`ThemeInfo` mirrors the Go `models.Theme` JSON:

```json
{
  "name": "Komari",
  "short": "default",
  "description": "Komari default theme",
  "version": "1.0.0",
  "author": "Akizon77",
  "url": "https://github.com/Fearless743/komari-web",
  "preview": "preview.png",
  "configuration": { "type": "managed", "data": [] }
}
```

`name` and `short` are required for metadata/API use. `list_themes` returns an
empty list when `data/theme` is absent (Go parity); otherwise it returns the
embedded default first, then valid directories below `data/theme`, skipping
unreadable or invalid configs with a warning. Preview lookup tries `preview`,
then `preview.png`, then `perview.png`, and prefers files owned by the theme
before falling back to the embedded default.

## Known limitations and integration responsibilities

* `configuration` is kept as opaque JSON; theme settings persistence belongs to
  the API/database crates.
* Theme upload, import, deletion and update (Go `api/admin/theme.go`) are not
  implemented here.
* The API/database layer owns persistence of the selected theme. Call
  `try_set_theme` after saving the config row; `set_theme` remains an unchecked
  restore helper.
* Temporary share links need the server layer to load
  `tempory_share_token` / `tempory_share_token_expire_at` from the database and
  call `set_temp_share_token` (or `clear_temp_share_token`). The fallback then
  sets the `temp_key` cookie; without that wiring, share links will not work.
  This is B03/B09 integration work, not a missing fallback path.
* `list_themes` does not create `data/theme`; the server should create it during
  startup, as Go's `public` init does.
* Pre-compressed variants are only served if the theme package ships them; the
  crate does not compress on the fly.
* `Range` requests are not implemented; static assets are returned whole.
* The embedded theme is selected at compile time, so changing
  `KOMARI_DEFAULT_THEME_DIR` requires a rebuild.
* The placeholder documentation lives at `crates/komari-web/PLACEHOLDER_THEME.md`,
  outside `defaultTheme/`, so it is not embedded or publicly reachable.

## Tests

The suite builds temporary themes and does not depend on the embedded theme's
file names or contents. Run it against both the committed placeholder and a
real frontend build:

```sh
# Placeholder theme (fresh checkout)
cargo test -p komari-web
cargo clippy -p komari-web --all-targets -- -D warnings

# Real frontend build
KOMARI_DEFAULT_THEME_DIR=/path/to/komari-web cargo test -p komari-web
KOMARI_DEFAULT_THEME_DIR=/path/to/komari-web \
  cargo clippy -p komari-web --all-targets -- -D warnings
```

CI should run the second pair when the frontend checkout is available.

