//! `cloudflared` tunnel management.
//!
//! This module ports `utils/cloudflared/cloudflared.go`:
//!
//! * choose the release asset for the current OS/architecture;
//! * download it to `data/cloudflared[.exe]` (or reuse an existing file);
//! * run `cloudflared tunnel --no-autoupdate run --token <token>`;
//! * forward its stdout/stderr line by line with a `[cloudflared]` prefix;
//! * stop the process on shutdown.
//!
//! Unlike Go, an unexpected exit is reported through [`Cloudflared::exited`]
//! instead of calling `os.Exit(1)`. The server assembly watches that future and
//! shuts down gracefully.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, watch};
use tracing::{error, info, warn};

/// Environment variable read by Go and `komari-config`.
pub const ENV_CLOUDFLARED_TOKEN: &str = komari_config::keys::ENV_CLOUDFLARED_TOKEN;

/// Time [`Cloudflared::kill`] waits for the child to be reaped.
pub const KILL_TIMEOUT: Duration = Duration::from_secs(5);

/// Release archive format used by a platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveFormat {
    /// The downloaded file is the executable itself (Linux/Windows).
    Raw,
    /// The executable is stored inside a gzip-compressed tar archive (macOS).
    TarGz,
}

/// A downloadable `cloudflared` release asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudflaredTarget {
    pub os: &'static str,
    pub arch: &'static str,
    pub url: &'static str,
    pub archive: ArchiveFormat,
}

impl CloudflaredTarget {
    /// File name inside `data/`, matching Go.
    pub fn file_name(&self) -> &'static str {
        if matches!(self.os, "windows") {
            "cloudflared.exe"
        } else {
            "cloudflared"
        }
    }
}

/// Normalize Rust's `std::env::consts` names to Go's `GOOS`/`GOARCH` names.
fn normalize_os(os: &str) -> &str {
    match os {
        "macos" => "darwin",
        other => other,
    }
}

fn normalize_arch(arch: &str) -> &str {
    match arch {
        "x86_64" => "amd64",
        "x86" => "386",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Select the release asset for `os`/`arch`.
///
/// Both Go names (`linux`, `amd64`, `386`, `darwin`) and Rust names (`macos`,
/// `x86`, `aarch64`) are accepted. Unsupported combinations return
/// [`CloudflaredError::UnsupportedPlatform`], matching Go's errors.
pub fn target_for(os: &str, arch: &str) -> Result<CloudflaredTarget, CloudflaredError> {
    let requested_os = normalize_os(os);
    let requested_arch = normalize_arch(arch);
    let (os, arch, url, archive) = match (requested_os, requested_arch) {
        ("windows", "amd64") => (
            "windows",
            "amd64",
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-amd64.exe",
            ArchiveFormat::Raw,
        ),
        ("windows", "386") => (
            "windows",
            "386",
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-386.exe",
            ArchiveFormat::Raw,
        ),
        ("linux", "amd64") => (
            "linux",
            "amd64",
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64",
            ArchiveFormat::Raw,
        ),
        ("linux", "386") => (
            "linux",
            "386",
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-386",
            ArchiveFormat::Raw,
        ),
        ("linux", "arm") => (
            "linux",
            "arm",
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-arm",
            ArchiveFormat::Raw,
        ),
        ("linux", "arm64") => (
            "linux",
            "arm64",
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-arm64",
            ArchiveFormat::Raw,
        ),
        ("darwin", "amd64") => (
            "darwin",
            "amd64",
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-amd64.tgz",
            ArchiveFormat::TarGz,
        ),
        ("darwin", "arm64") => (
            "darwin",
            "arm64",
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-arm64.tgz",
            ArchiveFormat::TarGz,
        ),
        _ => {
            return Err(CloudflaredError::UnsupportedPlatform {
                os: requested_os.to_owned(),
                arch: requested_arch.to_owned(),
            });
        }
    };
    Ok(CloudflaredTarget {
        os,
        arch,
        url,
        archive,
    })
}

/// Asset for the running process.
pub fn current_target() -> Result<CloudflaredTarget, CloudflaredError> {
    target_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// Downloads a release asset to `dest`.
#[async_trait]
pub trait Downloader: Send + Sync {
    async fn download_to(&self, url: &str, dest: &Path) -> Result<(), CloudflaredError>;
}

/// Default downloader backed by `reqwest`.
#[derive(Clone, Debug)]
pub struct HttpDownloader {
    client: reqwest::Client,
}

impl HttpDownloader {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("komari/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }
}

impl Default for HttpDownloader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Downloader for HttpDownloader {
    async fn download_to(&self, url: &str, dest: &Path) -> Result<(), CloudflaredError> {
        let response =
            self.client
                .get(url)
                .send()
                .await
                .map_err(|source| CloudflaredError::Download {
                    url: url.to_owned(),
                    message: source.to_string(),
                })?;
        let status = response.status();
        if !status.is_success() {
            return Err(CloudflaredError::Download {
                url: url.to_owned(),
                message: format!("HTTP status {status}"),
            });
        }

        let mut file =
            tokio::fs::File::create(dest)
                .await
                .map_err(|source| CloudflaredError::Install {
                    path: dest.to_path_buf(),
                    source,
                })?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|source| CloudflaredError::Download {
                url: url.to_owned(),
                message: source.to_string(),
            })?;
            file.write_all(&chunk)
                .await
                .map_err(|source| CloudflaredError::Install {
                    path: dest.to_path_buf(),
                    source,
                })?;
        }
        file.flush()
            .await
            .map_err(|source| CloudflaredError::Install {
                path: dest.to_path_buf(),
                source,
            })?;
        Ok(())
    }
}

/// Configuration for one tunnel.
#[derive(Clone)]
pub struct CloudflaredConfig {
    /// Directory that receives `cloudflared` / `cloudflared.exe`.
    pub data_dir: PathBuf,
    /// Tunnel token. Never logged or included in `Debug` output.
    pub token: String,
    /// Override the platform asset (tests and cross-platform installs).
    pub target: Option<CloudflaredTarget>,
    downloader: Arc<dyn Downloader>,
}

impl CloudflaredConfig {
    pub fn new(data_dir: impl Into<PathBuf>, token: impl Into<String>) -> Self {
        Self {
            data_dir: data_dir.into(),
            token: token.into(),
            target: None,
            downloader: Arc::new(HttpDownloader::default()),
        }
    }

    pub fn with_target(mut self, target: CloudflaredTarget) -> Self {
        self.target = Some(target);
        self
    }

    pub fn with_downloader(mut self, downloader: Arc<dyn Downloader>) -> Self {
        self.downloader = downloader;
        self
    }

    fn resolved_target(&self) -> Result<CloudflaredTarget, CloudflaredError> {
        match &self.target {
            Some(target) => Ok(target.clone()),
            None => current_target(),
        }
    }
}

impl fmt::Debug for CloudflaredConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CloudflaredConfig")
            .field("data_dir", &self.data_dir)
            .field("token", &"<redacted>")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

/// One recorded process exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudflaredExit {
    pub success: bool,
    pub code: Option<i32>,
}

#[derive(Debug, Error)]
pub enum CloudflaredError {
    #[error("unsupported cloudflared platform {os}/{arch}")]
    UnsupportedPlatform { os: String, arch: String },
    #[error("cloudflared token is empty")]
    EmptyToken,
    #[error("failed to create cloudflared data directory {path}: {source}")]
    DataDir { path: PathBuf, source: io::Error },
    #[error("failed to download cloudflared from {url}: {message}")]
    Download { url: String, message: String },
    #[error("failed to read cloudflared archive {path}: {source}")]
    Archive { path: PathBuf, source: io::Error },
    #[error("cloudflared archive {path} does not contain a cloudflared binary")]
    ArchiveMissingBinary { path: PathBuf },
    #[error("failed to install cloudflared at {path}: {source}")]
    Install { path: PathBuf, source: io::Error },
    #[error("failed to start cloudflared at {path}: {source}")]
    Spawn { path: PathBuf, source: io::Error },
}

/// Download (unless already present) and install the executable.
///
/// The file is written to `data/cloudflared[.exe]`; non-Windows targets are
/// made executable with mode `0755`. An existing file is reused without
/// contacting the network.
pub async fn install_binary(
    config: &CloudflaredConfig,
    target: &CloudflaredTarget,
) -> Result<PathBuf, CloudflaredError> {
    tokio::fs::create_dir_all(&config.data_dir)
        .await
        .map_err(|source| CloudflaredError::DataDir {
            path: config.data_dir.clone(),
            source,
        })?;

    let binary_path = config.data_dir.join(target.file_name());
    if tokio::fs::try_exists(&binary_path)
        .await
        .map_err(|source| CloudflaredError::Install {
            path: binary_path.clone(),
            source,
        })?
    {
        info!(
            path = %binary_path.display(),
            "reusing existing cloudflared binary"
        );
        return Ok(binary_path);
    }

    let download_path = config
        .data_dir
        .join(format!(".{}.download", target.file_name()));
    // A previous interrupted download must not be mistaken for an archive.
    let _ = tokio::fs::remove_file(&download_path).await;

    info!(url = target.url, "downloading cloudflared");
    config
        .downloader
        .download_to(target.url, &download_path)
        .await?;

    let install_result = match target.archive {
        ArchiveFormat::Raw => tokio::fs::rename(&download_path, &binary_path)
            .await
            .map_err(|source| CloudflaredError::Install {
                path: binary_path.clone(),
                source,
            }),
        ArchiveFormat::TarGz => extract_tgz_binary(&download_path, &binary_path),
    };
    let _ = tokio::fs::remove_file(&download_path).await;
    install_result?;

    if !matches!(target.os, "windows") {
        make_executable(&binary_path).map_err(|source| CloudflaredError::Install {
            path: binary_path.clone(),
            source,
        })?;
    }

    Ok(binary_path)
}

fn extract_tgz_binary(archive_path: &Path, binary_path: &Path) -> Result<(), CloudflaredError> {
    let file = std::fs::File::open(archive_path).map_err(|source| CloudflaredError::Archive {
        path: archive_path.to_path_buf(),
        source,
    })?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let entries = archive
        .entries()
        .map_err(|source| CloudflaredError::Archive {
            path: archive_path.to_path_buf(),
            source,
        })?;

    let extracted_path = binary_path.with_extension("extracted");
    let _ = std::fs::remove_file(&extracted_path);
    for entry in entries {
        let mut entry = entry.map_err(|source| CloudflaredError::Archive {
            path: archive_path.to_path_buf(),
            source,
        })?;
        let path = entry.path().map_err(|source| CloudflaredError::Archive {
            path: archive_path.to_path_buf(),
            source,
        })?;
        if path.file_name().and_then(|name| name.to_str()) != Some("cloudflared") {
            continue;
        }
        let mut output =
            std::fs::File::create(&extracted_path).map_err(|source| CloudflaredError::Install {
                path: extracted_path.clone(),
                source,
            })?;
        io::copy(&mut entry, &mut output).map_err(|source| CloudflaredError::Install {
            path: extracted_path.clone(),
            source,
        })?;
        drop(output);
        std::fs::rename(&extracted_path, binary_path).map_err(|source| {
            CloudflaredError::Install {
                path: binary_path.to_path_buf(),
                source,
            }
        })?;
        return Ok(());
    }

    Err(CloudflaredError::ArchiveMissingBinary {
        path: archive_path.to_path_buf(),
    })
}

fn make_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

struct Inner {
    kill_tx: Mutex<Option<oneshot::Sender<()>>>,
    exit_rx: watch::Receiver<bool>,
    exit_info: Arc<Mutex<Option<CloudflaredExit>>>,
    pid: Option<u32>,
}

/// A running `cloudflared` child process.
#[derive(Clone)]
pub struct Cloudflared {
    inner: Arc<Inner>,
}

impl fmt::Debug for Cloudflared {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Cloudflared")
            .field("pid", &self.inner.pid)
            .field("exited", &self.has_exited())
            .finish()
    }
}

impl Cloudflared {
    /// Install (if needed) and start `cloudflared`.
    ///
    /// An empty token is rejected before any download or spawn.
    pub async fn start(config: CloudflaredConfig) -> Result<Self, CloudflaredError> {
        let token = config.token.trim().to_owned();
        if token.is_empty() {
            return Err(CloudflaredError::EmptyToken);
        }
        let target = config.resolved_target()?;
        let binary_path = install_binary(&config, &target).await?;

        let mut command = Command::new(&binary_path);
        command
            .args(["tunnel", "--no-autoupdate", "run", "--token"])
            .arg(&token)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // If the supervisor task is aborted (for example while the Tokio
            // runtime is shutting down), Tokio still kills the child.
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|source| CloudflaredError::Spawn {
            path: binary_path.clone(),
            source,
        })?;
        let pid = child.id();

        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(stdout, token.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, token.clone());
        }

        let (kill_tx, kill_rx) = oneshot::channel();
        let (exit_tx, exit_rx) = watch::channel(false);
        let exit_info = Arc::new(Mutex::new(None));
        tokio::spawn(supervise(child, kill_rx, exit_tx, Arc::clone(&exit_info)));

        info!(pid = ?pid, "cloudflared started");
        Ok(Self {
            inner: Arc::new(Inner {
                kill_tx: Mutex::new(Some(kill_tx)),
                exit_rx,
                exit_info,
                pid,
            }),
        })
    }

    /// Process id, if the child is still owned by this handle.
    pub fn pid(&self) -> Option<u32> {
        self.inner.pid
    }

    /// Whether the child has exited (or was never running).
    pub fn has_exited(&self) -> bool {
        *self.inner.exit_rx.borrow()
    }

    /// Exit status recorded by the supervisor, once available.
    pub fn exit_info(&self) -> Option<CloudflaredExit> {
        *self
            .inner
            .exit_info
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Resolve when the child exits.
    pub async fn exited(&self) {
        let mut receiver = self.inner.exit_rx.clone();
        if *receiver.borrow() {
            return;
        }
        while receiver.changed().await.is_ok() {
            if *receiver.borrow() {
                return;
            }
        }
    }

    /// Ask the supervisor to kill the child. Safe to call repeatedly.
    pub fn request_kill(&self) {
        let sender = self
            .inner
            .kill_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(sender) = sender {
            let _ = sender.send(());
        }
    }

    /// Kill the child and wait until it has been reaped.
    ///
    /// This is idempotent: after the first call, subsequent calls return
    /// immediately.
    pub async fn kill(&self) {
        self.request_kill();
        let mut receiver = self.inner.exit_rx.clone();
        if *receiver.borrow() {
            return;
        }
        let waited = tokio::time::timeout(KILL_TIMEOUT, async {
            while receiver.changed().await.is_ok() {
                if *receiver.borrow() {
                    return;
                }
            }
        })
        .await;
        if waited.is_err() {
            warn!(
                timeout = ?KILL_TIMEOUT,
                "timed out waiting for cloudflared to stop"
            );
        }
    }
}

// Dropping the last `Cloudflared` handle drops the oneshot sender inside
// `Inner`; the supervisor treats that as a kill request. Cloned handles can
// therefore be dropped independently without stopping the tunnel.

fn spawn_log_reader<R>(reader: R, token: String)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => info!("[cloudflared] {}", redact_secret(&line, &token)),
                Ok(None) => break,
                Err(source) => {
                    warn!(%source, "failed to read cloudflared output");
                    break;
                }
            }
        }
    });
}

/// Replace the tunnel token in child output. `cloudflared` does not print the
/// token today, but forwarding arbitrary output must never leak it.
fn redact_secret(line: &str, token: &str) -> String {
    if token.is_empty() {
        line.to_owned()
    } else {
        line.replace(token, "<redacted>")
    }
}

async fn supervise(
    mut child: Child,
    kill_rx: oneshot::Receiver<()>,
    exit_tx: watch::Sender<bool>,
    exit_info: Arc<Mutex<Option<CloudflaredExit>>>,
) {
    let status = tokio::select! {
        _ = kill_rx => {
            if let Err(source) = child.start_kill() {
                warn!(%source, "failed to kill cloudflared");
            }
            child.wait().await
        }
        status = child.wait() => status,
    };

    match status {
        Ok(status) => {
            let info = CloudflaredExit {
                success: status.success(),
                code: status.code(),
            };
            *exit_info
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(info);
            if info.success {
                info!(code = ?info.code, "[cloudflared] exited successfully");
            } else {
                error!(code = ?info.code, "[cloudflared] exited with an error");
            }
        }
        Err(source) => {
            error!(%source, "[cloudflared] failed to wait for the process");
        }
    }
    let _ = exit_tx.send(true);
}

#[cfg(test)]
mod tests {
    use super::redact_secret;

    #[test]
    fn redacts_the_token_from_child_output() {
        let line = "tunnel token=super-secret-token ready";
        let redacted = redact_secret(line, "super-secret-token");
        assert!(!redacted.contains("super-secret-token"), "{redacted}");
        assert!(redacted.contains("<redacted>"), "{redacted}");
    }

    #[test]
    fn leaves_output_unchanged_without_a_token() {
        assert_eq!(redact_secret("plain line", ""), "plain line");
    }
}
