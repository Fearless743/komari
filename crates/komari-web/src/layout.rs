//! Validation of a theme directory layout.
//!
//! `build.rs` performs the same checks before `rust-embed` bakes the assets
//! into the binary. The public helper is also useful for deployments that load
//! a theme from disk and want a clear error instead of a missing file later.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;

/// Paths of the files a theme package must provide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeLayout {
    pub root: PathBuf,
    pub dist: PathBuf,
    pub index: PathBuf,
    pub config: PathBuf,
    pub preview: PathBuf,
}

#[derive(Debug, Error)]
pub enum ThemeLayoutError {
    #[error("theme directory does not exist: {0}")]
    MissingDirectory(PathBuf),
    #[error("theme directory has no `dist/` directory: {0}")]
    MissingDist(PathBuf),
    #[error("theme `dist/` has no `index.html`: {0}")]
    MissingIndex(PathBuf),
    #[error("theme has no `komari-theme.json`: {0}")]
    MissingConfig(PathBuf),
    #[error("theme has no preview image (`preview.png` or `perview.png`) in {0}")]
    MissingPreview(PathBuf),
    #[error("cannot inspect theme path {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Validate that `dir` contains a complete, servable theme.
///
/// Returns the resolved paths on success. A missing directory is reported as
/// [`ThemeLayoutError::MissingDirectory`] rather than a generic I/O error so
/// callers can print actionable guidance.
pub fn validate_theme_layout(dir: impl AsRef<Path>) -> Result<ThemeLayout, ThemeLayoutError> {
    let root = dir.as_ref().to_path_buf();

    match fs::metadata(&root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(ThemeLayoutError::MissingDirectory(root));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(ThemeLayoutError::MissingDirectory(root));
        }
        Err(err) => {
            return Err(ThemeLayoutError::Io {
                path: root,
                source: err,
            });
        }
    }

    let dist = root.join("dist");
    match fs::metadata(&dist) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(ThemeLayoutError::MissingDist(dist)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(ThemeLayoutError::MissingDist(dist));
        }
        Err(err) => {
            return Err(ThemeLayoutError::Io {
                path: dist,
                source: err,
            });
        }
    }

    let index = dist.join("index.html");
    if !index.is_file() {
        return Err(ThemeLayoutError::MissingIndex(index));
    }

    let config = root.join("komari-theme.json");
    if !config.is_file() {
        return Err(ThemeLayoutError::MissingConfig(config));
    }

    let preview = ["preview.png", "perview.png"]
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| ThemeLayoutError::MissingPreview(root.clone()))?;

    Ok(ThemeLayout {
        root,
        dist,
        index,
        config,
        preview,
    })
}
