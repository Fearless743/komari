//! Static asset representation, content types, cache policy and HTTP encoding.

use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{HeaderMap, Method, StatusCode, header},
    response::Response,
};
use bytes::Bytes;

/// Cache policy for a resolved asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    /// Content-addressed file below `assets/`; safe to cache forever.
    Immutable,
    /// `index.html`; must be revalidated so new deployments are picked up.
    Index,
    /// Other files (manifest, service worker, favicon, ...).
    Asset,
}

impl CachePolicy {
    /// Classify a theme-relative path (`dist/assets/app-abc123.js`, ...).
    pub fn for_path(path: &str) -> Self {
        let normalized = path.trim_start_matches('/');
        if normalized == "dist/index.html" || normalized.ends_with("/index.html") {
            Self::Index
        } else if normalized.contains("/assets/") || normalized.starts_with("assets/") {
            Self::Immutable
        } else {
            Self::Asset
        }
    }

    pub fn header_value(
        self,
        immutable_max_age: u64,
        index_max_age: u64,
        asset_max_age: u64,
    ) -> String {
        match self {
            Self::Immutable => format!("public, max-age={immutable_max_age}, immutable"),
            Self::Index => format!("public, max-age={index_max_age}, must-revalidate"),
            Self::Asset => format!("public, max-age={asset_max_age}"),
        }
    }
}

/// A resolved file ready to be turned into an HTTP response.
#[derive(Debug, Clone)]
pub struct Asset {
    pub bytes: Bytes,
    pub content_type: String,
    pub etag: String,
    pub cache_control: String,
    /// `Some("br")` / `Some("gzip")` for a pre-compressed variant.
    pub encoding: Option<&'static str>,
    pub last_modified: Option<SystemTime>,
}

impl Asset {
    pub fn new(
        bytes: Bytes,
        path: &str,
        cache_control: impl Into<String>,
        encoding: Option<&'static str>,
        last_modified: Option<SystemTime>,
    ) -> Self {
        Self {
            etag: etag_for(&bytes, encoding),
            content_type: content_type_for(path),
            bytes,
            cache_control: cache_control.into(),
            encoding,
            last_modified,
        }
    }

    /// Build the response, honouring `HEAD` and `If-None-Match`.
    pub fn into_response(self, method: &Method, request_headers: &HeaderMap) -> Response {
        let is_head = method == Method::HEAD;

        if let Some(value) = request_headers
            .get(header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
        {
            if etag_matches(value, &self.etag) {
                let mut builder = Response::builder()
                    .status(StatusCode::NOT_MODIFIED)
                    .header(header::ETAG, self.etag.as_str())
                    .header(header::CACHE_CONTROL, self.cache_control.as_str())
                    .header(header::VARY, "Accept-Encoding");
                if let Some(encoding) = self.encoding {
                    builder = builder.header(header::CONTENT_ENCODING, encoding);
                }
                return builder.body(Body::empty()).expect("valid 304 response");
            }
        }

        let mut builder = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, self.content_type.as_str())
            .header(header::CONTENT_LENGTH, self.bytes.len())
            .header(header::CACHE_CONTROL, self.cache_control.as_str())
            .header(header::ETAG, self.etag.as_str())
            .header(header::VARY, "Accept-Encoding");

        if let Some(encoding) = self.encoding {
            builder = builder.header(header::CONTENT_ENCODING, encoding);
        }
        if let Some(last_modified) = self.last_modified {
            builder = builder.header(
                header::LAST_MODIFIED,
                httpdate::fmt_http_date(last_modified),
            );
        }

        let body = if is_head {
            Body::empty()
        } else {
            Body::from(self.bytes)
        };
        builder.body(body).expect("valid asset response")
    }
}

/// Return the `Content-Type` for a theme-relative path.
pub fn content_type_for(path: &str) -> String {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let (mime, charset) = match extension.as_str() {
        "html" | "htm" => ("text/html", true),
        "js" | "mjs" => ("text/javascript", true),
        "css" => ("text/css", true),
        "json" | "map" => ("application/json", false),
        "webmanifest" => ("application/manifest+json", false),
        "txt" => ("text/plain", true),
        "xml" => ("text/xml", true),
        "svg" => ("image/svg+xml", false),
        "png" => ("image/png", false),
        "jpg" | "jpeg" => ("image/jpeg", false),
        "gif" => ("image/gif", false),
        "webp" => ("image/webp", false),
        "avif" => ("image/avif", false),
        "ico" => ("image/vnd.microsoft.icon", false),
        "woff" => ("font/woff", false),
        "woff2" => ("font/woff2", false),
        "ttf" => ("font/ttf", false),
        "otf" => ("font/otf", false),
        "wasm" => ("application/wasm", false),
        "pdf" => ("application/pdf", false),
        "mp4" => ("video/mp4", false),
        "webm" => ("video/webm", false),
        "mp3" => ("audio/mpeg", false),
        _ => {
            let guessed = mime_guess::from_path(path).first_or_octet_stream();
            return guessed.essence_str().to_owned();
        }
    };

    if charset {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_owned()
    }
}

/// Compute a strong ETag from the payload. The encoding is part of the tag
/// because the same URL can serve identity, gzip and brotli representations.
pub fn etag_for(bytes: &[u8], encoding: Option<&str>) -> String {
    let hash = fnv1a64(bytes);
    match encoding {
        Some(encoding) => format!("\"{hash:016x}-{:x}-{encoding}\"", bytes.len()),
        None => format!("\"{hash:016x}-{:x}\"", bytes.len()),
    }
}

/// Whether an `If-None-Match` header matches `etag` (weak comparison).
pub fn etag_matches(header_value: &str, etag: &str) -> bool {
    header_value.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*"
            || candidate == etag
            || candidate
                .strip_prefix("W/")
                .is_some_and(|weak| weak == etag)
    })
}

/// Quality value for `coding` in `Accept-Encoding`.
///
/// An explicit entry wins over `*`; a missing header (or no matching entry and
/// no wildcard) returns `None`. Invalid `q` values are treated as `0`.
pub fn encoding_quality(headers: &HeaderMap, coding: &str) -> Option<f32> {
    let value = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())?;

    let mut wildcard = None;
    for entry in value.split(',') {
        let mut parts = entry.split(';');
        let name = parts.next().unwrap_or_default().trim();
        if name.is_empty() {
            continue;
        }

        let quality = parts
            .find_map(|parameter| {
                let (key, value) = parameter.split_once('=')?;
                key.trim()
                    .eq_ignore_ascii_case("q")
                    .then(|| parse_quality(value))
            })
            .unwrap_or(1.0);

        if name.eq_ignore_ascii_case(coding) {
            return Some(quality);
        }
        if name == "*" {
            wildcard = Some(quality);
        }
    }

    wildcard
}

/// Pick the highest-quality coding from `codings`, preserving the order of
/// `codings` as the tie-breaker.
pub fn preferred_encoding<'a>(headers: &HeaderMap, codings: &[&'a str]) -> Option<&'a str> {
    let mut best: Option<(&'a str, f32)> = None;

    for &coding in codings {
        let Some(quality) = encoding_quality(headers, coding) else {
            continue;
        };
        if quality <= 0.0 {
            continue;
        }
        if best.is_none_or(|(_, best_quality)| quality > best_quality) {
            best = Some((coding, quality));
        }
    }

    best.map(|(coding, _)| coding)
}

/// All acceptable codings ordered by descending quality, preserving the input
/// order for ties. Codings with `q=0` are omitted.
pub fn accepted_encodings<'a>(headers: &HeaderMap, codings: &[&'a str]) -> Vec<&'a str> {
    let mut candidates: Vec<(&'a str, f32)> = codings
        .iter()
        .filter_map(|&coding| {
            encoding_quality(headers, coding)
                .filter(|quality| *quality > 0.0)
                .map(|quality| (coding, quality))
        })
        .collect();

    // `sort_by` is stable, so equal qualities keep the caller's preference
    // order (br before gzip for us).
    candidates.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    candidates.into_iter().map(|(coding, _)| coding).collect()
}

/// Whether the request accepts `coding` (`gzip`, `br`, ...).
pub fn accepts_encoding(headers: &HeaderMap, coding: &str) -> bool {
    encoding_quality(headers, coding).is_some_and(|quality| quality > 0.0)
}

fn parse_quality(value: &str) -> f32 {
    value
        .trim()
        .parse::<f32>()
        .map(|quality| quality.clamp(0.0, 1.0))
        .unwrap_or(0.0)
}

/// `SystemTime` for a rust-embed last-modified timestamp.
pub(crate) fn system_time_from_unix(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}
