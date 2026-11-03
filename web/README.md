# Frontend assets placeholder

The Rust backend embeds/serves the web UI from this directory tree. Frontend
integration is owned by task **B14**; until then `web/dist` only contains a
`.gitkeep` so Docker builds keep working.

Expected layout once B14 lands:

```text
web/
└── dist/          # Vite/NPM production output (index.html, assets/, ...)
```

Docker builds copy `web/dist` into the image (`/app/web`). To use a different
build output, pass the path relative to the repository root:

```sh
docker build --build-arg KOMARI_WEB_DIST=web/dist .
```

CI runs the optional frontend build step only when `web/package.json` exists.
