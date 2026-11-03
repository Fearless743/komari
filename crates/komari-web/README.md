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

## Serving behaviour (Go parity)

| Request | Behaviour |
| --- | --- |
| `/favicon.ico` | `data/favicon.ico`, then the active theme's `dist/favicon.ico`, then the embedded default, else 404 |
| `/themes/{id}/{path}` | `data/theme/{id}/{path}` first, then the embedded default theme |
| unknown `GET`/`HEAD` path | active theme's `dist{path}`, otherwise `dist/index.html` |
| `/admin*` | default theme's `index.html`, no site replacements |
| `/api`, `/api/*` | always 404, never the SPA |
| other methods on unknown paths | 404 |

Theme selection uses the configured theme short name. If
`data/theme/<theme>/komari-theme.json` is missing or invalid, `default` is used.
Files missing from an installed theme fall back to the embedded default on a
per-file basis, matching the Go `getFileContent` helper.

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
* Optional pre-compressed `.br` / `.gz` siblings are served when the client
  accepts them, with `Content-Encoding` and `Vary: Accept-Encoding`. Disable
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
assets.set_theme("my-theme");
assets.set_site_meta(SiteMeta {
    sitename: "My Monitor".into(),
    description: "status page".into(),
    custom_head: String::new(),
    custom_body: String::new(),
});

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

`name` and `short` are required. `list_themes` returns the embedded default
first, then valid directories below `data/theme`, skipping unreadable or
invalid configs with a warning. Preview lookup tries `preview`, then
`preview.png`, then `perview.png`.

## Known limitations

* `configuration` is kept as opaque JSON; theme settings persistence belongs to
  the API/database crates.
* Theme upload, import, deletion and update (Go `api/admin/theme.go`) are not
  implemented here.
* Pre-compressed variants are only served if the theme package ships them; the
  crate does not compress on the fly.
* `Range` requests are not implemented; static assets are returned whole.
* The embedded theme is selected at compile time, so changing
  `KOMARI_DEFAULT_THEME_DIR` requires a rebuild.

## Tests

```sh
cargo test -p komari-web
cargo clippy -p komari-web --all-targets -- -D warnings
```
