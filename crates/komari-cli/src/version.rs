//! Version information.
//!
//! `CARGO_PKG_VERSION` is used by default. Release builds can override the
//! version and commit hash at compile time:
//!
//! ```text
//! KOMARI_VERSION=1.2.3 KOMARI_VERSION_HASH=abcdef0 cargo build --release
//! ```

use komari_config::LogLevel;

/// Build version. `KOMARI_VERSION` wins over the Cargo package version.
pub const VERSION: &str = match option_env!("KOMARI_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Git commit hash. `unknown` identifies a development build, matching the Go
/// backend's `utils.VersionHash`.
pub const VERSION_HASH: &str = match option_env!("KOMARI_VERSION_HASH") {
    Some(hash) => hash,
    None => "unknown",
};

/// Whether this is a development build without a version hash.
pub fn is_development() -> bool {
    VERSION_HASH == "unknown"
}

/// The Go backend logs at debug level while the hash is `unknown` and at info
/// level for release builds.
pub fn default_log_level() -> LogLevel {
    if is_development() {
        LogLevel::Debug
    } else {
        LogLevel::Info
    }
}

/// `version (hash: hash)` used in the startup banner and `--version` output.
pub fn long_version() -> String {
    format!("{VERSION} (hash: {VERSION_HASH})")
}
