//! `cloudflared` asset selection, installation and process lifecycle tests.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use komari_api::server::{
    ArchiveFormat, BackgroundConfig, Cloudflared, CloudflaredConfig, CloudflaredError, Downloader,
    ServerBuilder, ServerConfig, install_binary, target_for,
};
use komari_config::Config;
use komari_db::Db;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Clone, Default)]
struct MockDownloader {
    bytes: Vec<u8>,
    fail: bool,
    partial_failure: bool,
    calls: Arc<AtomicUsize>,
}

impl MockDownloader {
    fn bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
            fail: false,
            partial_failure: false,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn failing() -> Self {
        Self {
            bytes: Vec::new(),
            fail: true,
            partial_failure: false,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn partial_failure() -> Self {
        Self {
            bytes: Vec::new(),
            fail: false,
            partial_failure: true,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Downloader for MockDownloader {
    async fn download_to(&self, url: &str, dest: &Path) -> Result<(), CloudflaredError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(CloudflaredError::Download {
                url: url.to_owned(),
                message: "mock download failure".to_owned(),
            });
        }
        if self.partial_failure {
            tokio::fs::write(dest, b"partial bytes")
                .await
                .map_err(|source| CloudflaredError::Install {
                    path: dest.to_path_buf(),
                    source,
                })?;
            return Err(CloudflaredError::Download {
                url: url.to_owned(),
                message: "connection dropped after partial write".to_owned(),
            });
        }
        tokio::fs::write(dest, &self.bytes)
            .await
            .map_err(|source| CloudflaredError::Install {
                path: dest.to_path_buf(),
                source,
            })
    }
}

fn config(dir: &TempDir, downloader: MockDownloader) -> CloudflaredConfig {
    CloudflaredConfig::new(dir.path(), "test-token")
        .with_target(target_for("linux", "amd64").expect("linux target"))
        .with_downloader(Arc::new(downloader))
}

const RELEASE_PREFIX: &str = "https://github.com/cloudflare/cloudflared/releases/latest/download/";

#[test]
fn selects_linux_assets_for_every_architecture() {
    let expected = [
        ("amd64", "cloudflared-linux-amd64"),
        ("386", "cloudflared-linux-386"),
        ("arm", "cloudflared-linux-arm"),
        ("arm64", "cloudflared-linux-arm64"),
    ];
    for (arch, asset) in expected {
        let target = target_for("linux", arch).expect("supported linux target");
        assert_eq!(target.os, "linux");
        assert_eq!(target.arch, arch);
        assert_eq!(target.archive, ArchiveFormat::Raw);
        assert_eq!(target.file_name(), "cloudflared");
        assert_eq!(target.url, format!("{RELEASE_PREFIX}{asset}"));
    }
}

#[test]
fn selects_windows_assets_and_exe_name() {
    for (arch, asset) in [
        ("amd64", "cloudflared-windows-amd64.exe"),
        ("386", "cloudflared-windows-386.exe"),
    ] {
        let target = target_for("windows", arch).expect("supported windows target");
        assert_eq!(target.file_name(), "cloudflared.exe");
        assert_eq!(target.archive, ArchiveFormat::Raw);
        assert_eq!(target.url, format!("{RELEASE_PREFIX}{asset}"));
    }
}

#[test]
fn selects_darwin_tgz_assets() {
    for (arch, asset) in [
        ("amd64", "cloudflared-darwin-amd64.tgz"),
        ("arm64", "cloudflared-darwin-arm64.tgz"),
    ] {
        let target = target_for("darwin", arch).expect("supported darwin target");
        assert_eq!(target.archive, ArchiveFormat::TarGz);
        assert_eq!(target.file_name(), "cloudflared");
        assert_eq!(target.url, format!("{RELEASE_PREFIX}{asset}"));
    }
}

#[test]
fn normalizes_rust_platform_names() {
    assert_eq!(target_for("macos", "aarch64").expect("macos").os, "darwin");
    assert_eq!(target_for("macos", "aarch64").expect("macos").arch, "arm64");
    assert_eq!(
        target_for("linux", "x86_64").expect("linux x86_64").arch,
        "amd64"
    );
    assert_eq!(target_for("linux", "x86").expect("linux x86").arch, "386");
}

#[test]
fn rejects_unsupported_platforms() {
    for (os, arch) in [
        ("linux", "mips"),
        ("windows", "arm64"),
        ("darwin", "386"),
        ("freebsd", "amd64"),
    ] {
        let error = target_for(os, arch).expect_err("unsupported platform");
        assert!(
            matches!(error, CloudflaredError::UnsupportedPlatform { .. }),
            "{error:?}"
        );
    }
}

#[tokio::test]
async fn reuses_an_existing_binary_without_downloading() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("cloudflared");
    std::fs::write(&path, b"already installed").expect("write existing binary");
    let downloader = MockDownloader::bytes(b"replacement");
    let config = config(&dir, downloader.clone());

    let installed = install_binary(&config, &config.target.clone().expect("target"))
        .await
        .expect("install");

    assert_eq!(installed, path);
    assert_eq!(downloader.calls(), 0);
    assert_eq!(std::fs::read(&path).expect("read"), b"already installed");
}

#[tokio::test]
async fn download_failure_is_reported() {
    let dir = TempDir::new().expect("temp dir");
    let downloader = MockDownloader::failing();
    let config = config(&dir, downloader);

    let error = install_binary(&config, &config.target.clone().expect("target"))
        .await
        .expect_err("download failure");

    match error {
        CloudflaredError::Download { message, .. } => {
            assert!(message.contains("mock download failure"), "{message}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(!dir.path().join("cloudflared").exists());
}

#[tokio::test]
async fn partial_download_failure_removes_temporary_files() {
    let dir = TempDir::new().expect("temp dir");
    let downloader = MockDownloader::partial_failure();
    let config = config(&dir, downloader);

    let error = install_binary(&config, &config.target.clone().expect("target"))
        .await
        .expect_err("partial download failure");

    assert!(
        matches!(error, CloudflaredError::Download { .. }),
        "{error:?}"
    );
    assert!(!dir.path().join("cloudflared").exists());
    assert!(!dir.path().join(".cloudflared.download").exists());
}

#[tokio::test]
async fn failed_archive_extraction_removes_temporary_files() {
    let dir = TempDir::new().expect("temp dir");
    let target = target_for("darwin", "arm64").expect("darwin target");
    let downloader = MockDownloader::bytes(b"this is not a gzip archive");
    let config = CloudflaredConfig::new(dir.path(), "test-token")
        .with_target(target.clone())
        .with_downloader(Arc::new(downloader));

    let error = install_binary(&config, &target)
        .await
        .expect_err("invalid archive");

    assert!(
        matches!(error, CloudflaredError::Archive { .. }),
        "{error:?}"
    );
    assert!(!dir.path().join("cloudflared").exists());
    assert!(!dir.path().join(".cloudflared.download").exists());
    assert!(!dir.path().join("cloudflared.extracted").exists());
}

#[tokio::test]
async fn installs_a_raw_binary_and_sets_mode() {
    let dir = TempDir::new().expect("temp dir");
    let downloader = MockDownloader::bytes(b"#!/bin/sh\nexit 0\n");
    let config = config(&dir, downloader);

    let installed = install_binary(&config, &config.target.clone().expect("target"))
        .await
        .expect("install");

    assert_eq!(
        std::fs::read(&installed).expect("read"),
        b"#!/bin/sh\nexit 0\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&installed)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
    }
}

#[tokio::test]
async fn installs_windows_binary_as_exe_without_execute_bit() {
    let dir = TempDir::new().expect("temp dir");
    let downloader = MockDownloader::bytes(b"MZ");
    let target = target_for("windows", "amd64").expect("windows target");
    let config = CloudflaredConfig::new(dir.path(), "test-token")
        .with_target(target.clone())
        .with_downloader(Arc::new(downloader));

    let installed = install_binary(&config, &target).await.expect("install");

    assert_eq!(
        installed.file_name().and_then(|name| name.to_str()),
        Some("cloudflared.exe")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&installed)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0, "Windows assets must not be chmodded");
    }
}

fn tgz_with_binary(contents: &[u8]) -> Vec<u8> {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "cloudflared", contents)
            .expect("append binary");
        builder.finish().expect("finish tar");
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).expect("compress");
    encoder.finish().expect("finish gzip")
}

#[tokio::test]
async fn extracts_the_darwin_tgz_binary() {
    let dir = TempDir::new().expect("temp dir");
    let target = target_for("darwin", "arm64").expect("darwin target");
    let downloader = MockDownloader::bytes(tgz_with_binary(b"darwin binary"));
    let config = CloudflaredConfig::new(dir.path(), "test-token")
        .with_target(target.clone())
        .with_downloader(Arc::new(downloader));

    let installed = install_binary(&config, &target).await.expect("install");

    assert_eq!(std::fs::read(&installed).expect("read"), b"darwin binary");
    assert!(!dir.path().join(".cloudflared.download").exists());
}

#[tokio::test]
async fn empty_token_is_rejected_before_downloading() {
    let dir = TempDir::new().expect("temp dir");
    let downloader = MockDownloader::bytes(b"binary");
    let config = CloudflaredConfig::new(dir.path(), "   ")
        .with_target(target_for("linux", "amd64").expect("target"))
        .with_downloader(Arc::new(downloader.clone()));

    let error = Cloudflared::start(config).await.expect_err("empty token");

    assert!(matches!(error, CloudflaredError::EmptyToken), "{error:?}");
    assert_eq!(downloader.calls(), 0);
    assert!(!dir.path().join("cloudflared").exists());
}

#[test]
fn debug_output_redacts_the_token() {
    let config = CloudflaredConfig::new("data", "super-secret-token");
    let debug = format!("{config:?}");
    assert!(!debug.contains("super-secret-token"), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");

    let app = Config {
        cloudflared_token: Some("super-secret-token".to_owned()),
        ..Config::default()
    };
    let debug = format!("{app:?}");
    assert!(!debug.contains("super-secret-token"), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");

    let server = ServerConfig {
        cloudflared_token: Some("super-secret-token".to_owned()),
        ..ServerConfig::default()
    };
    let debug = format!("{server:?}");
    assert!(!debug.contains("super-secret-token"), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");
}

#[cfg(unix)]
fn write_mock_script(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("cloudflared");
    std::fs::write(&path, body).expect("write mock script");
    let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("chmod mock script");
    path
}

#[cfg(unix)]
fn mock_config(dir: &TempDir) -> CloudflaredConfig {
    CloudflaredConfig::new(dir.path(), "test-token")
        .with_target(target_for("linux", "amd64").expect("target"))
        .with_downloader(Arc::new(MockDownloader::failing()))
}

#[cfg(unix)]
#[tokio::test]
async fn starts_and_kills_a_mock_process() {
    let dir = TempDir::new().expect("temp dir");
    write_mock_script(
        dir.path(),
        "#!/bin/sh\necho mock-stdout\necho mock-stderr >&2\nwhile :; do sleep 1; done\n",
    );
    let process = Cloudflared::start(mock_config(&dir))
        .await
        .expect("start mock cloudflared");

    assert!(process.pid().is_some());
    assert!(!process.has_exited());

    tokio::time::timeout(Duration::from_secs(5), process.kill())
        .await
        .expect("kill timeout");
    assert!(process.has_exited());
    assert!(process.exit_info().is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn kill_is_idempotent() {
    let dir = TempDir::new().expect("temp dir");
    write_mock_script(dir.path(), "#!/bin/sh\nwhile :; do sleep 1; done\n");
    let process = Cloudflared::start(mock_config(&dir))
        .await
        .expect("start mock cloudflared");

    process.kill().await;
    tokio::time::timeout(Duration::from_secs(1), process.kill())
        .await
        .expect("second kill should return immediately");
    assert!(process.has_exited());
}

#[cfg(unix)]
#[tokio::test]
async fn records_an_unexpected_exit() {
    let dir = TempDir::new().expect("temp dir");
    write_mock_script(dir.path(), "#!/bin/sh\nexit 3\n");
    let process = Cloudflared::start(mock_config(&dir))
        .await
        .expect("start mock cloudflared");

    tokio::time::timeout(Duration::from_secs(5), process.exited())
        .await
        .expect("process should exit");

    let info = process.exit_info().expect("recorded exit");
    assert!(!info.success);
    assert_eq!(info.code, Some(3));
}

fn memory_db() -> Db {
    let db = Db::open_in_memory().expect("open in-memory database");
    db.migrate().expect("migrate database");
    db
}

fn server_config(dir: &TempDir) -> ServerConfig {
    ServerConfig {
        listen: "127.0.0.1:0".to_owned(),
        database_path: dir.path().join("komari.db"),
        data_dir: dir.path().to_path_buf(),
        background: BackgroundConfig {
            enabled: false,
            ..BackgroundConfig::default()
        },
        request_logging: false,
        ..ServerConfig::default()
    }
}

#[tokio::test]
async fn server_without_a_token_does_not_start_a_tunnel() {
    let dir = TempDir::new().expect("temp dir");
    let server = ServerBuilder::new(server_config(&dir))
        .with_db(memory_db())
        .skip_services()
        .build()
        .await
        .expect("build server");

    assert!(server.cloudflared().is_none());
}

#[tokio::test]
async fn server_with_cloudflared_disabled_ignores_the_token() {
    let dir = TempDir::new().expect("temp dir");
    let config = ServerConfig {
        enable_cloudflared: false,
        cloudflared_token: Some("test-token".to_owned()),
        ..server_config(&dir)
    };
    let server = ServerBuilder::new(config)
        .with_db(memory_db())
        .skip_services()
        .build()
        .await
        .expect("build server");

    assert!(server.cloudflared().is_none());
}

#[tokio::test]
async fn server_with_enabled_cloudflared_and_empty_token_does_not_start() {
    for token in [None, Some(String::new()), Some("   ".to_owned())] {
        let dir = TempDir::new().expect("temp dir");
        let config = ServerConfig {
            enable_cloudflared: true,
            cloudflared_token: token,
            ..server_config(&dir)
        };
        let server = ServerBuilder::new(config)
            .with_db(memory_db())
            .skip_services()
            .build()
            .await
            .expect("build server");

        assert!(server.cloudflared().is_none());
    }
}

#[test]
fn from_app_config_maps_the_cloudflared_gate() {
    let mut app = Config::default();
    app.enable_cloudflared = true;
    app.cloudflared_token = Some("app-token".to_owned());

    let server = ServerConfig::from_app_config(&app);
    assert!(server.enable_cloudflared);
    assert_eq!(server.cloudflared_token.as_deref(), Some("app-token"));

    app.enable_cloudflared = false;
    let server = ServerConfig::from_app_config(&app);
    assert!(!server.enable_cloudflared);
    assert_eq!(server.cloudflared_token.as_deref(), Some("app-token"));
}

#[cfg(unix)]
#[tokio::test]
async fn tunnel_exit_triggers_a_fast_graceful_shutdown_and_an_error() {
    let dir = TempDir::new().expect("temp dir");
    write_mock_script(dir.path(), "#!/bin/sh\nsleep 0.1\nexit 3\n");
    let config = ServerConfig {
        enable_cloudflared: true,
        cloudflared_token: Some("test-token".to_owned()),
        shutdown_timeout: Duration::from_secs(2),
        ..server_config(&dir)
    };
    let server = ServerBuilder::new(config)
        .with_db(memory_db())
        .skip_services()
        .build()
        .await
        .expect("build server");
    let cloudflared = server
        .cloudflared()
        .expect("tunnel should be configured")
        .clone();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind random port");

    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        server.serve_with_shutdown(listener, std::future::pending()),
    )
    .await
    .expect("server should shut down before the timeout");
    let elapsed = started.elapsed();

    let error = result.expect_err("cloudflared exit must fail the server");
    assert!(error.to_string().contains("cloudflared"), "{error}");
    assert!(
        elapsed < Duration::from_secs(1),
        "tunnel exit should drain immediately, took {elapsed:?}"
    );
    assert!(cloudflared.has_exited());
    assert_eq!(cloudflared.exit_info().and_then(|info| info.code), Some(3));
}

#[cfg(unix)]
#[tokio::test]
async fn server_shutdown_stops_the_configured_tunnel() {
    let dir = TempDir::new().expect("temp dir");
    write_mock_script(dir.path(), "#!/bin/sh\nwhile :; do sleep 1; done\n");
    let config = ServerConfig {
        enable_cloudflared: true,
        cloudflared_token: Some("test-token".to_owned()),
        ..server_config(&dir)
    };
    let server = ServerBuilder::new(config)
        .with_db(memory_db())
        .skip_services()
        .build()
        .await
        .expect("build server");
    let cloudflared = server
        .cloudflared()
        .expect("tunnel should be configured")
        .clone();
    assert!(!cloudflared.has_exited());

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind random port");
    let (shutdown, receiver) = oneshot::channel();
    let handle = tokio::spawn(server.serve_with_shutdown(listener, async {
        let _ = receiver.await;
    }));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!cloudflared.has_exited());

    let _ = shutdown.send(());
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("server shutdown timeout")
        .expect("server task")
        .expect("serve");
    assert!(cloudflared.has_exited());
}
