//! Resolve and validate the default theme directory that `rust-embed` bakes
//! into the binary.
//!
//! The directory layout is the one produced by the `komari-web` frontend
//! (`npm run build` plus `komari-theme.json` and `preview.png`):
//!
//! ```text
//! <theme>/
//!   komari-theme.json
//!   preview.png        (or perview.png, the historical spelling)
//!   dist/
//!     index.html
//!     assets/...
//! ```
//!
//! `KOMARI_DEFAULT_THEME_DIR` may point at a real frontend build. When it is
//! unset the placeholder theme committed under `crates/komari-web/defaultTheme`
//! is used so the crate always builds in a fresh checkout.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process,
};

const ENV_VAR: &str = "KOMARI_DEFAULT_THEME_DIR";

fn main() {
    println!("cargo:rerun-if-env-changed={ENV_VAR}");

    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by cargo"),
    );
    let configured = env::var_os(ENV_VAR).filter(|value| !value.is_empty());
    let (theme_dir, is_placeholder) = match configured {
        Some(value) => {
            let path = PathBuf::from(value);
            let path = if path.is_absolute() {
                path
            } else {
                manifest_dir.join(path)
            };
            (path, false)
        }
        None => (manifest_dir.join("defaultTheme"), true),
    };

    if let Err(message) = validate(&theme_dir) {
        eprintln!("\nerror: komari-web cannot embed the default theme.\n\n{message}\n");
        eprintln!(
            "Build the frontend first, then point `{ENV_VAR}` at the directory that contains\n\
             `dist/`, `komari-theme.json` and `preview.png`:\n\n    \
             cd komari-web && npm run build\n    \
             {ENV_VAR}=/path/to/komari-web cargo build -p komari-web\n"
        );
        process::exit(1);
    }

    if is_placeholder {
        println!(
            "cargo:warning=komari-web: embedding the placeholder theme in crates/komari-web/defaultTheme; \
             set {ENV_VAR}=/path/to/komari-web to embed a real frontend build"
        );
    }

    println!("cargo:rerun-if-changed={}", theme_dir.display());
    println!(
        "cargo:rustc-env=KOMARI_EMBEDDED_THEME_DIR={}",
        theme_dir.display()
    );
}

fn validate(dir: &Path) -> Result<(), String> {
    let metadata = fs::metadata(dir).map_err(|err| {
        format!(
            "  theme directory does not exist or cannot be read: {}\n  reason: {err}",
            dir.display()
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "  theme path is not a directory: {}",
            dir.display()
        ));
    }

    let dist = dir.join("dist");
    match fs::metadata(&dist) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(format!("  `dist` is not a directory: {}", dist.display()));
        }
        Err(_) => {
            return Err(format!(
                "  missing frontend build output: {}\n  run `npm run build` in komari-web first",
                dist.display()
            ));
        }
    }

    let index = dist.join("index.html");
    if !index.is_file() {
        return Err(format!(
            "  missing SPA entry point: {}\n  run `npm run build` in komari-web first",
            index.display()
        ));
    }

    let config = dir.join("komari-theme.json");
    if !config.is_file() {
        return Err(format!("  missing theme metadata: {}", config.display()));
    }

    let has_preview = dir.join("preview.png").is_file() || dir.join("perview.png").is_file();
    if !has_preview {
        return Err(format!(
            "  missing theme preview image: expected `preview.png` or `perview.png` in {}",
            dir.display()
        ));
    }

    Ok(())
}
