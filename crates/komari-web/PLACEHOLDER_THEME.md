# Placeholder default theme

This directory keeps `cargo build -p komari-web` working in a fresh checkout.
The real default theme is produced by the `komari-web` frontend repository:

```sh
cd komari-web
npm run build
```

Point the backend at that checkout when building:

```sh
KOMARI_DEFAULT_THEME_DIR=/path/to/komari-web cargo build -p komari-web
```

The directory must contain `dist/`, `komari-theme.json`, and `preview.png`
(or the historical `perview.png` spelling). See `../README.md`.
