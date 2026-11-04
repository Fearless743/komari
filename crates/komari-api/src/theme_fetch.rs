//! Remote theme downloads (`theme/import`, `theme/update`).
//!
//! The Go backend downloads theme packages with the default `http.Client` and
//! guards against SSRF by resolving the host and rejecting private/internal
//! addresses. This module keeps the same error messages and status handling,
//! but adds the limits the Go code lacks:
//!
//! * a request/connect timeout;
//! * a download size cap;
//! * redirect targets are re-validated, so a public URL cannot redirect to an
//!   internal address.
//!
//! [`ThemeFetcher`] is injected through [`ApiState`](crate::ApiState) so tests
//! can exercise the success paths without network access.

use std::fmt::{self, Display};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::LOCATION;
use reqwest::redirect::Policy;
use reqwest::{Client, Url};
use tokio::net::lookup_host;

/// Maximum number of bytes accepted for an uploaded or downloaded theme
/// archive. The Go backend has no limit; 64 MiB is far above real themes while
/// still bounding memory use.
pub const THEME_ARCHIVE_LIMIT: usize = 64 * 1024 * 1024;
const MAX_REDIRECTS: usize = 10;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A theme download / GitHub release lookup failure.
///
/// The message is already formatted like the Go errors (`下载主题文件失败: …`),
/// so handlers only need to add their operation prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeFetchError {
    message: String,
}

impl ThemeFetchError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for ThemeFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ThemeFetchError {}

/// Fetches theme archives and GitHub release metadata.
///
/// The default implementation is [`HttpThemeFetcher`]; tests install a fake
/// with [`ApiState::with_theme_fetcher`](crate::ApiState::with_theme_fetcher).
#[async_trait]
pub trait ThemeFetcher: Send + Sync {
    /// Download a ZIP archive from an `http`/`https` URL.
    async fn download(&self, url: &str) -> Result<Vec<u8>, ThemeFetchError>;

    /// Resolve the first asset of a GitHub repository's latest release.
    async fn github_release(&self, owner: &str, repo: &str) -> Result<String, ThemeFetchError>;
}

/// Real HTTP implementation with SSRF protection and size/time limits.
#[derive(Debug, Clone)]
pub struct HttpThemeFetcher {
    client: Result<Client, String>,
}

impl Default for HttpThemeFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpThemeFetcher {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(Policy::none())
            .user_agent(concat!("komari/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| error.to_string());
        Self { client }
    }

    async fn get(&self, url: &str, profile: ErrorProfile) -> Result<Vec<u8>, ThemeFetchError> {
        let client = match &self.client {
            Ok(client) => client,
            Err(error) => return Err(profile.request(error)),
        };

        let mut current = validate_url(url).await?;
        for hop in 0..=MAX_REDIRECTS {
            let response = client
                .get(current.clone())
                .send()
                .await
                .map_err(|error| profile.request(error))?;
            let status = response.status();

            // Go's `http.Client` follows these statuses; other 3xx responses
            // (for example 304) fall through to the non-200 error.
            if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
                if hop == MAX_REDIRECTS {
                    return Err(profile.request("too many redirects"));
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        profile.request("redirect response without a Location header")
                    })?;
                // Re-resolve and re-check the redirect target: Go's client
                // would follow it blindly, which is an SSRF bypass.
                current = resolve_redirect(&current, location, profile).await?;
                continue;
            }

            if status != reqwest::StatusCode::OK {
                return Err(profile.status(status.as_u16()));
            }

            let mut data = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| profile.read(error))?;
                if data.len().saturating_add(chunk.len()) > THEME_ARCHIVE_LIMIT {
                    return Err(profile.too_large());
                }
                data.extend_from_slice(&chunk);
            }
            if data.is_empty() {
                return Err(profile.empty());
            }
            return Ok(data);
        }

        Err(profile.request("too many redirects"))
    }
}

#[async_trait]
impl ThemeFetcher for HttpThemeFetcher {
    async fn download(&self, url: &str) -> Result<Vec<u8>, ThemeFetchError> {
        self.get(url, ErrorProfile::Theme).await
    }

    async fn github_release(&self, owner: &str, repo: &str) -> Result<String, ThemeFetchError> {
        if owner.is_empty() || repo.is_empty() {
            return Err(ThemeFetchError::new("GitHub仓库所有者和仓库名称不能为空"));
        }
        let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
        let data = self.get(&url, ErrorProfile::Github).await?;
        let release: ReleaseInfo = serde_json::from_slice(&data)
            .map_err(|error| ThemeFetchError::new(format!("解析GitHub API响应失败: {error}")))?;
        release
            .assets
            .into_iter()
            .next()
            .map(|asset| asset.browser_download_url)
            .ok_or_else(|| ThemeFetchError::new("GitHub release中没有可下载的资源"))
    }
}

#[derive(serde::Deserialize)]
struct ReleaseInfo {
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(serde::Deserialize)]
struct ReleaseAsset {
    #[serde(default)]
    browser_download_url: String,
}

#[derive(Clone, Copy)]
enum ErrorProfile {
    Theme,
    Github,
}

impl ErrorProfile {
    fn request(self, error: impl Display) -> ThemeFetchError {
        match self {
            Self::Theme => ThemeFetchError::new(format!("下载主题文件失败: {error}")),
            Self::Github => ThemeFetchError::new(format!("获取GitHub release信息失败: {error}")),
        }
    }

    fn status(self, status: u16) -> ThemeFetchError {
        match self {
            Self::Theme => ThemeFetchError::new(format!("下载主题文件失败，HTTP状态码: {status}")),
            Self::Github => {
                ThemeFetchError::new(format!("获取GitHub release信息失败，HTTP状态码: {status}"))
            }
        }
    }

    fn read(self, error: impl Display) -> ThemeFetchError {
        match self {
            Self::Theme => ThemeFetchError::new(format!("读取主题文件内容失败: {error}")),
            Self::Github => ThemeFetchError::new(format!("读取GitHub release信息失败: {error}")),
        }
    }

    fn too_large(self) -> ThemeFetchError {
        match self {
            Self::Theme => ThemeFetchError::new(format!(
                "下载的主题文件过大（超过 {} MiB）",
                THEME_ARCHIVE_LIMIT / (1024 * 1024)
            )),
            Self::Github => ThemeFetchError::new("GitHub release信息过大"),
        }
    }

    fn empty(self) -> ThemeFetchError {
        match self {
            Self::Theme => ThemeFetchError::new("下载的主题文件为空"),
            Self::Github => ThemeFetchError::new("解析GitHub API响应失败: empty response"),
        }
    }
}

/// Resolve a `Location` header and re-run the scheme/host checks.
///
/// `current` is always the URL of the previous hop, so relative redirects work
/// while the final target still has to be a public `http`/`https` URL.
async fn resolve_redirect(
    current: &Url,
    location: &str,
    profile: ErrorProfile,
) -> Result<Url, ThemeFetchError> {
    let next = current
        .join(location)
        .map_err(|error| profile.request(format!("invalid redirect URL: {error}")))?;
    validate_url(next.as_str()).await
}

/// Parse and SSRF-check a download URL.
async fn validate_url(raw: &str) -> Result<Url, ThemeFetchError> {
    let url =
        Url::parse(raw).map_err(|error| ThemeFetchError::new(format!("invalid URL: {error}")))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(ThemeFetchError::new(
            "only http and https schemes are allowed",
        ));
    }
    ensure_public_host(&url).await?;
    Ok(url)
}

/// Reject hosts that resolve to loopback, private, link-local or otherwise
/// non-public addresses. DNS failures fail closed, like Go's `isPrivateIP`.
async fn ensure_public_host(url: &Url) -> Result<(), ThemeFetchError> {
    let host = url
        .host_str()
        .ok_or_else(|| ThemeFetchError::new("invalid URL: missing host"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');

    if let Ok(ip) = host.parse::<IpAddr>() {
        return if is_private_ip(ip) {
            Err(private_error())
        } else {
            Ok(())
        };
    }

    let port = url.port_or_known_default().unwrap_or(80);
    let addresses = lookup_host((host, port))
        .await
        .map_err(|_| private_error())?;
    let mut resolved = false;
    for address in addresses {
        resolved = true;
        if is_private_ip(address.ip()) {
            return Err(private_error());
        }
    }
    if resolved {
        Ok(())
    } else {
        Err(private_error())
    }
}

fn private_error() -> ThemeFetchError {
    ThemeFetchError::new("requests to private/internal addresses are not allowed")
}

/// Whether an address is non-public and must not be fetched.
///
/// Covers Go's `IsLoopback`/`IsPrivate`/`IsLinkLocal*` checks plus unspecified,
/// multicast, broadcast and CGNAT ranges. IPv4-mapped IPv6 addresses are
/// checked as IPv4.
pub(crate) fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_private_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_private_v4(mapped);
            }
            let segments = ip.segments();
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (segments[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
                || (segments[0] & 0xffc0) == 0xfe80 // fe80::/10 link local
        }
    }
}

fn is_private_v4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || (octets[0] == 100 && (octets[1] & 0xc0) == 64) // 100.64.0.0/10 CGNAT
}

/// Detect a GitHub repository URL and return `(owner, repo)`.
///
/// Mirrors Go's `isGitHubRepoURL`: the string must contain `github.com`, the
/// authority must be exactly `github.com`/`www.github.com` (no port), and the
/// path must have at least two segments. A trailing `.git` is stripped once.
pub(crate) fn github_repo(raw: &str) -> Option<(String, String)> {
    if raw.is_empty() || !raw.to_ascii_lowercase().contains("github.com") {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    // Go compares `parsedURL.Host`, which keeps an explicit port (even the
    // scheme's default port), so `github.com:443` is not treated as a
    // repository URL. `Url::port()` normalizes default ports away.
    if url.port().is_some() || authority_has_port(raw) {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    if host != "github.com" && host != "www.github.com" {
        return None;
    }

    let path = url.path().trim_start_matches('/');
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    Some((owner.to_owned(), repo.to_owned()))
}

fn authority_has_port(raw: &str) -> bool {
    let Some((_, rest)) = raw.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or(authority);
    host.matches(':').count() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_addresses_are_rejected() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_private_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(!is_private_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn github_urls_match_go() {
        assert_eq!(
            github_repo("https://github.com/owner/repo"),
            Some(("owner".into(), "repo".into()))
        );
        assert_eq!(
            github_repo("https://www.github.com/owner/repo.git"),
            Some(("owner".into(), "repo".into()))
        );
        assert_eq!(
            github_repo("http://github.com/owner/repo?x=github.com"),
            Some(("owner".into(), "repo".into()))
        );
        assert_eq!(github_repo("https://github.com:443/owner/repo"), None);
        assert_eq!(github_repo("https://gist.github.com/owner/repo"), None);
        assert_eq!(github_repo("https://example.com/github.com/owner"), None);
        assert_eq!(github_repo(""), None);
    }

    #[tokio::test]
    async fn redirect_targets_are_revalidated() {
        let current = Url::parse("https://1.1.1.1/theme.zip").unwrap();

        // Relative redirects stay on the same public host.
        let next = resolve_redirect(&current, "/other.zip", ErrorProfile::Theme)
            .await
            .expect("relative redirect");
        assert_eq!(next.as_str(), "https://1.1.1.1/other.zip");

        for target in [
            "http://127.0.0.1/theme.zip",
            "http://192.168.1.10/theme.zip",
            "http://[::1]/theme.zip",
        ] {
            let error = resolve_redirect(&current, target, ErrorProfile::Theme)
                .await
                .expect_err("private redirect target");
            assert!(
                error.message().contains("private/internal addresses"),
                "{target}: {error}"
            );
        }

        let error = resolve_redirect(&current, "file:///tmp/theme.zip", ErrorProfile::Theme)
            .await
            .expect_err("non-http redirect target");
        assert!(error.message().contains("only http and https"), "{error}");
    }

    #[tokio::test]
    async fn unsupported_schemes_are_rejected() {
        let error = validate_url("file:///tmp/theme.zip")
            .await
            .expect_err("file URLs must be rejected");
        assert_eq!(error.message(), "only http and https schemes are allowed");
    }
}
