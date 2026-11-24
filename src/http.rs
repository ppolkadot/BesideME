//! Single point of execution for all site HTTP GET requests.
//!
//! Enforces descriptive User-Agent, total + connect timeouts, max 2 redirects,
//! HTTPS-only (with loopback HTTP allowed for local tests), conditional GET (`If-None-Match`),
//! and a streaming 5 MiB response size cap.

use std::net::IpAddr;
use std::time::Duration;

use reqwest::header::{ETAG, IF_NONE_MATCH, RETRY_AFTER};
use reqwest::{Client, StatusCode, Url, redirect};
use thiserror::Error;

/// Hard cap on site HTTP response body size (5 MiB).
pub const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

/// Fixed TCP/TLS connect timeout (10 seconds).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum number of HTTP redirects followed when fetching a series page.
pub const MAX_REDIRECTS: usize = 2;

/// Errors returned when fetching a manga series page.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HttpError {
    /// Request exceeded the configured total or connect timeout.
    #[error("request timed out")]
    Timeout,

    /// TCP/TLS connection or DNS resolution failure.
    #[error("connection error: {0}")]
    Connect(String),

    /// Target or redirect URL violated the HTTPS-only policy.
    #[error("insecure scheme rejected for URL `{0}` (only https is allowed)")]
    InsecureScheme(String),

    /// Redirect chain exceeded [`MAX_REDIRECTS`].
    #[error("too many redirects (max {MAX_REDIRECTS})")]
    TooManyRedirects,

    /// Server returned HTTP `403` or `429` (rate-limited or blocked).
    #[error("HTTP {status} (blocked or rate-limited)")]
    BlockedOrRateLimited {
        /// HTTP status code (`403` or `429`).
        status: u16,
        /// Optional `Retry-After` header value in seconds.
        retry_after_secs: Option<u64>,
    },

    /// Server returned a `5xx` error.
    #[error("server error: HTTP {status}")]
    ServerError {
        /// HTTP status code (`500..=599`).
        status: u16,
    },

    /// Server returned an unexpected non-2xx status (e.g. `404`).
    #[error("unexpected HTTP status {status}")]
    UnexpectedStatus {
        /// HTTP status code.
        status: u16,
    },

    /// Response body exceeded [`MAX_RESPONSE_BYTES`] (5 MiB).
    #[error("response body exceeded size cap of {limit} bytes")]
    ResponseTooLarge {
        /// Byte limit that was exceeded.
        limit: usize,
    },

    /// Other transport or body-streaming error.
    #[error("network error: {0}")]
    Transport(String),
}

impl HttpError {
    /// Short classification label used in logs and health alerts.
    #[must_use]
    pub fn kind_label(&self) -> String {
        match self {
            Self::Timeout => "timeout".to_string(),
            Self::Connect(_) => "connection error".to_string(),
            Self::InsecureScheme(_) => "insecure scheme".to_string(),
            Self::TooManyRedirects => "too many redirects".to_string(),
            Self::BlockedOrRateLimited { status, .. } => format!("HTTP {status}"),
            Self::ServerError { status } | Self::UnexpectedStatus { status } => {
                format!("HTTP {status}")
            }
            Self::ResponseTooLarge { .. } => "response too large (>5 MB)".to_string(),
            Self::Transport(_) => "network transport error".to_string(),
        }
    }
}

/// Successful result of a conditional or unconditional site GET.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Server returned `304 Not Modified` matching our `If-None-Match` `ETag`.
    NotModified {
        /// Updated or preserved `ETag` header.
        etag: Option<String>,
    },
    /// Server returned `200 OK` with the HTML document.
    Fetched {
        /// Full UTF-8 response body (guaranteed `<= 5 MiB`).
        body: String,
        /// Optional `ETag` response header for future conditional GETs.
        etag: Option<String>,
    },
}

/// HTTP client dedicated to fetching series pages with strict politeness and safety limits.
#[derive(Debug, Clone)]
pub struct SiteHttpClient {
    client: Client,
}

impl SiteHttpClient {
    /// Builds a new [`SiteHttpClient`] with the given `user_agent` and total request `timeout`.
    pub fn new(user_agent: &str, timeout: Duration) -> Result<Self, HttpError> {
        let redirect_policy = redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            if !is_allowed_url_scheme(attempt.url()) {
                return attempt.error("redirect to non-https URL is forbidden");
            }
            attempt.follow()
        });

        let client = Client::builder()
            .user_agent(user_agent)
            .timeout(timeout)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(redirect_policy)
            .build()
            .map_err(|e| HttpError::Transport(e.to_string()))?;

        Ok(Self { client })
    }

    /// Performs a single HTTP GET for `url`, optionally sending `If-None-Match: <etag>`.
    pub async fn fetch(&self, url: &Url, etag: Option<&str>) -> Result<FetchOutcome, HttpError> {
        if !is_allowed_url_scheme(url) {
            return Err(HttpError::InsecureScheme(url.to_string()));
        }

        let mut req = self.client.get(url.clone());
        if let Some(tag) = etag.map(str::trim).filter(|s| !s.is_empty()) {
            req = req.header(IF_NONE_MATCH, tag);
        }

        let mut response = req.send().await.map_err(|e| classify_reqwest_error(&e))?;
        let status = response.status();

        let response_etag = response
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| etag.map(ToString::to_string));

        if status == StatusCode::NOT_MODIFIED {
            return Ok(FetchOutcome::NotModified {
                etag: response_etag,
            });
        }

        if matches!(
            status,
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
        ) {
            let retry_after_secs = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok());
            return Err(HttpError::BlockedOrRateLimited {
                status: status.as_u16(),
                retry_after_secs,
            });
        }

        if status.is_server_error() {
            return Err(HttpError::ServerError {
                status: status.as_u16(),
            });
        }

        if !status.is_success() {
            return Err(HttpError::UnexpectedStatus {
                status: status.as_u16(),
            });
        }

        if let Some(content_len) = response.content_length()
            && content_len > MAX_RESPONSE_BYTES as u64
        {
            return Err(HttpError::ResponseTooLarge {
                limit: MAX_RESPONSE_BYTES,
            });
        }

        let mut buf = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| classify_reqwest_error(&e))?
        {
            if buf.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(HttpError::ResponseTooLarge {
                    limit: MAX_RESPONSE_BYTES,
                });
            }
            buf.extend_from_slice(&chunk);
        }

        let body = String::from_utf8(buf)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());

        Ok(FetchOutcome::Fetched {
            body,
            etag: response_etag,
        })
    }
}

fn is_allowed_url_scheme(url: &Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => url.host_str().is_some_and(is_loopback_host),
        _ => false,
    }
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    unbracketed
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

fn classify_reqwest_error(err: &reqwest::Error) -> HttpError {
    if err.is_timeout() {
        return HttpError::Timeout;
    }
    if err.is_redirect() {
        let msg = err.to_string();
        if msg.contains("non-https") {
            return HttpError::InsecureScheme(msg);
        }
        return HttpError::TooManyRedirects;
    }
    if err.is_connect() {
        return HttpError::Connect(err.to_string());
    }
    HttpError::Transport(err.to_string())
}
