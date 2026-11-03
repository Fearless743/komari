//! Small HTTP abstraction so provider request construction can be tested
//! without a network.
//!
//! Go providers call `net/http` directly. Keeping the request description
//! separate from `reqwest` lets tests capture the exact method, URL, headers
//! and body that a provider would send.

use std::time::Duration;

use async_trait::async_trait;

use crate::error::{NotifyError, Result};
use crate::redact::{is_sensitive_header, redact_url_for_debug};

/// A fully-resolved HTTP request.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers = self
            .headers
            .iter()
            .map(|(name, value)| {
                if is_sensitive_header(name) {
                    (name.as_str(), "<redacted>")
                } else {
                    (name.as_str(), value.as_str())
                }
            })
            .collect::<Vec<_>>();
        let body = self
            .body
            .as_ref()
            .map(|body| format!("<{} bytes redacted>", body.len()));
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &redact_url_for_debug(&self.url))
            .field("headers", &headers)
            .field("body", &body)
            .finish()
    }
}

impl HttpRequest {
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn get(url: impl Into<String>) -> Self {
        Self::new("GET", url)
    }

    pub fn post(url: impl Into<String>) -> Self {
        Self::new("POST", url)
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Replace every existing header with the same (case-insensitive) name,
    /// matching Go's `http.Header.Set`.
    pub fn set_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let name = name.into();
        self.headers
            .retain(|(existing, _)| !existing.eq_ignore_ascii_case(&name));
        self.headers.push((name, value.into()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Header lookup is case-insensitive, matching HTTP.
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn body_text(&self) -> Option<&str> {
        self.body
            .as_deref()
            .and_then(|body| std::str::from_utf8(body).ok())
    }
}

/// A raw HTTP response.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("body", &format!("<{} bytes redacted>", self.body.len()))
            .finish()
    }
}

impl HttpResponse {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            body: body.into(),
        }
    }

    pub fn ok_json(value: &serde_json::Value) -> Self {
        Self::new(200, serde_json::to_vec(value).unwrap_or_default())
    }

    pub fn body_text(&self) -> &str {
        std::str::from_utf8(&self.body).unwrap_or("")
    }
}

/// Executes an [`HttpRequest`].
#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse>;
}

/// The production transport backed by `reqwest`.
#[derive(Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
    timeout: Option<Duration>,
}

impl std::fmt::Debug for ReqwestTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReqwestTransport")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

impl ReqwestTransport {
    /// Build a transport with an overall request timeout.
    pub fn new(timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();
        Self {
            client,
            timeout: Some(timeout),
        }
    }

    /// Build a transport with no client-side timeout, like Go's
    /// `http.DefaultClient` used by the telegram and bark providers.
    pub fn without_timeout() -> Self {
        let client = reqwest::Client::builder().build().unwrap_or_default();
        Self {
            client,
            timeout: None,
        }
    }

    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|error| NotifyError::Http(format!("invalid method: {error}")))?;
        let mut builder = self.client.request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }

        let response = builder.send().await.map_err(reqwest_error)?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(reqwest_error)?.to_vec();
        Ok(HttpResponse { status, body })
    }
}

/// Map a reqwest error without ever including the request URL (which can
/// contain bot tokens, sendkeys or webhook path tokens).
fn reqwest_error(error: reqwest::Error) -> NotifyError {
    let kind = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_body() || error.is_decode() {
        "response body"
    } else {
        "request"
    };
    NotifyError::Http(format!("{kind} failed"))
}

/// Percent-encode one `application/x-www-form-urlencoded` component the same
/// way Go's `url.QueryEscape` does: spaces become `+`, unreserved characters
/// are kept, everything else is `%XX` with uppercase hex digits.
pub fn query_escape(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push('+'),
            _ => {
                use std::fmt::Write;
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

/// Encode ordered key/value pairs as a form body (`url.Values.Encode()`).
pub fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", query_escape(key), query_escape(value)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_escape_matches_go() {
        assert_eq!(query_escape("a b+c/d"), "a+b%2Bc%2Fd");
        assert_eq!(query_escape("a~b"), "a~b");
        assert_eq!(query_escape("中文"), "%E4%B8%AD%E6%96%87");
        assert_eq!(query_escape("line\nbreak"), "line%0Abreak");
    }

    #[test]
    fn timeout_matches_go_default_clients() {
        assert_eq!(
            ReqwestTransport::default().timeout(),
            Some(Duration::from_secs(30))
        );
        assert_eq!(ReqwestTransport::without_timeout().timeout(), None);
    }

    #[test]
    fn form_encode_keeps_pair_order() {
        assert_eq!(form_encode(&[("b", "2"), ("a", "1 2")]), "b=2&a=1+2");
    }
}
