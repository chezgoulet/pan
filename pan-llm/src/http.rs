//! # A tiny, std-only, blocking HTTP/1.0 JSON client — plain **and** TLS.
//!
//! Deliberately minimal: no async, and no HTTP framework. It builds one request
//! and reads one response, over either a plain `TcpStream` (`http://`, for local
//! OpenAI-compatible servers and the test mock) or a rustls TLS stream
//! (`https://`, for cloud BYOK — OpenAI, OpenRouter, Groq, Together, an
//! Anthropic-compatible endpoint). The request/response handling is identical
//! across both; only the byte transport differs.
//!
//! HTTP/1.0 is a deliberate choice, shared with `pan-daemon`'s `llm.rs`: a 1.0
//! request tells the server not to keep the connection alive or chunk-encode the
//! body, so "read to EOF, split head from body" is a correct, tiny parser. Cloud
//! edges (CloudFront et al.) honor a 1.0 request with `Connection: close`. TLS
//! peers that close without a `close_notify` surface `UnexpectedEof`, which we
//! treat as a clean end once the body is in hand.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use pan_core::schema::Value;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream as AsyncTcpStream;
use tokio::time::{sleep, timeout as tokio_timeout};

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Maximum retries on retryable HTTP statuses (429, 5xx).
const MAX_RETRIES: u32 = 3;

/// POST `body` as JSON to `base` + `path` and parse the JSON response. `api_key`,
/// if present, is sent as a bearer token. `http://` and `https://` bases are both
/// supported. Any transport/status/parse failure is a human-readable `Err(String)`
/// — the provider turns that into `Conclude(Abandoned)`.
///
/// Retries on 429 (rate limit) and 5xx (server error) with exponential backoff,
/// respecting a `Retry-After` header when present. Other errors (4xx, transport)
/// fail immediately.
/// POST `body` as JSON to `base` + `path`, adding `api_key` as a Bearer
/// Authorization header. Wraps [`post_json_ex`] with no extra headers.
pub fn post_json(
    base: &str,
    path: &str,
    api_key: Option<&str>,
    body: &Value,
    timeout: Duration,
) -> Result<Value, String> {
    let headers: &[(&str, String)] = &match api_key {
        Some(k) => vec![("Authorization", format!("Bearer {k}"))],
        None => vec![],
    };
    post_json_ex(base, path, body, headers, timeout)
}

/// POST `body` as JSON to `base` + `path` with arbitrary extra headers, then
/// parse the JSON response. `http://` and `https://` bases are both supported.
/// See [`post_json`] for retry/backoff semantics.
pub fn post_json_ex(
    base: &str,
    path: &str,
    body: &Value,
    extra_headers: &[(&str, String)],
    timeout: Duration,
) -> Result<Value, String> {
    let target = parse_base(base, path)?;
    let mut last_err: Option<(u16, String)> = None;
    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            let delay_ms = 500 * 2u64.pow(attempt - 1);
            std::thread::sleep(Duration::from_millis(delay_ms + rand_delay(delay_ms)));
        }
        let request = build_request_ex(&target.host, &target.full_path, body, extra_headers);
        let raw = match target.scheme {
            Scheme::Http => http_exchange(&target.host, target.port, timeout, request.as_bytes())?,
            Scheme::Https => {
                https_exchange(&target.host, target.port, timeout, request.as_bytes())?
            }
        };
        match parse_response(&raw) {
            Ok(val) => return Ok(val),
            Err((status, msg)) if status >= 429 => {
                let msg_clone = msg.clone();
                last_err = Some((status, msg));
                if let Some(ms) = parse_retry_after(&raw) {
                    std::thread::sleep(Duration::from_millis(ms));
                }
                if status == 429 || status >= 500 {
                    continue;
                }
                return Err(format!("HTTP {status}: {msg_clone}"));
            }
            Err((_, msg)) => return Err(msg),
        }
    }
    match last_err {
        Some((status, msg)) => Err(format!("HTTP {status} after {MAX_RETRIES} retries: {msg}")),
        None => Err("max retries exceeded".into()),
    }
}

/// Parse the `Retry-After` header from an HTTP response. Returns milliseconds, or
/// `None` if absent/unparseable. The RFC allows both seconds (int) and HTTP-date;
/// we handle the common case (seconds as an integer).
fn parse_retry_after(raw: &str) -> Option<u64> {
    let head = raw.split_once("\r\n\r\n")?.0;
    for line in head.lines() {
        if line.to_ascii_lowercase().starts_with("retry-after:") {
            let val = line.split(':').nth(1)?.trim();
            // Try seconds as integer
            if let Ok(secs) = val.parse::<u64>() {
                return Some(secs.min(30).saturating_mul(1000));
            }
        }
    }
    None
}

/// A small jitter: up to half the base delay (in ms).
fn rand_delay(base_ms: u64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (seed as u64) % (base_ms / 2 + 1)
}

// ---------------------------------------------------------------------------
// Request / response (transport-independent)
// ---------------------------------------------------------------------------

fn build_request_ex(
    host: &str,
    full_path: &str,
    body: &Value,
    extra_headers: &[(&str, String)],
) -> String {
    let payload = body.to_string();
    let mut request = format!(
        "POST {full_path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: pan/{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        env!("CARGO_PKG_VERSION"),
        payload.len()
    );
    for (name, value) in extra_headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&payload);
    request
}

fn parse_response(raw: &str) -> Result<Value, (u16, String)> {
    let (head, response_body) = raw
        .split_once("\r\n\r\n")
        .ok_or_else(|| (0, "malformed HTTP response".to_string()))?;
    let status_line = head.lines().next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| (0, format!("bad status line: {status_line:?}")))?;
    if status != 200 {
        return Err((
            status,
            response_body[..response_body.len().min(400)].to_string(),
        ));
    }
    serde_json::from_str(response_body).map_err(|e| (status, format!("bad response JSON: {e}")))
}

/// Read to EOF, tolerating a TLS peer that closes without a `close_notify`
/// (surfaced as `UnexpectedEof`) once we already have the response bytes.
fn read_to_end_tolerant(stream: &mut dyn Read) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_RESPONSE_BYTES {
                    break;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Transports
// ---------------------------------------------------------------------------

fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let stream = TcpStream::connect((host, port)).map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    Ok(stream)
}

fn http_exchange(
    host: &str,
    port: u16,
    timeout: Duration,
    request: &[u8],
) -> Result<String, String> {
    let mut stream = connect(host, port, timeout)?;
    stream
        .write_all(request)
        .map_err(|e| format!("send: {e}"))?;
    let bytes = read_to_end_tolerant(&mut stream)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn https_exchange(
    host: &str,
    port: u16,
    timeout: Duration,
    request: &[u8],
) -> Result<String, String> {
    let config = tls_config()?;
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| format!("invalid TLS server name {host:?}"))?;
    let mut conn = rustls::ClientConnection::new(config, server_name)
        .map_err(|e| format!("tls setup: {e}"))?;
    let mut sock = connect(host, port, timeout)?;
    let mut tls = rustls::Stream::new(&mut conn, &mut sock);
    tls.write_all(request)
        .map_err(|e| format!("tls send: {e}"))?;
    let bytes = read_to_end_tolerant(&mut tls)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The shared rustls client config (Mozilla roots, `ring` provider), built once.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(config.clone());
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("tls provider: {e}"))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let config = Arc::new(config);
    // A race here just discards one identical config; either wins.
    let _ = CONFIG.set(config.clone());
    Ok(config)
}

// ---------------------------------------------------------------------------
// URL parsing
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Scheme {
    Http,
    Https,
}

#[derive(Debug, PartialEq, Eq)]
struct Target {
    scheme: Scheme,
    host: String,
    port: u16,
    full_path: String,
}

/// Split `http(s)://host[:port][/prefix]` + a leading-slash `path` into a
/// [`Target`]. The port defaults by scheme (80 / 443).
fn parse_base(base: &str, path: &str) -> Result<Target, String> {
    let (scheme, rest, default_port) = if let Some(rest) = base.strip_prefix("https://") {
        (Scheme::Https, rest, 443)
    } else if let Some(rest) = base.strip_prefix("http://") {
        (Scheme::Http, rest, 80)
    } else {
        return Err(format!("base {base:?} must start with http:// or https://"));
    };

    let (authority, prefix) = match rest.split_once('/') {
        Some((a, p)) => (a, p.trim_end_matches('/')),
        None => (rest, ""),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| format!("bad port {p:?}"))?),
        None => (authority, default_port),
    };
    if host.is_empty() {
        return Err("empty host".into());
    }
    let full_path = if prefix.is_empty() {
        path.to_string()
    } else {
        format!("/{prefix}{path}")
    };
    Ok(Target {
        scheme,
        host: host.to_string(),
        port,
        full_path,
    })
}

// ---------------------------------------------------------------------------
// GET request (public, for version check and binary download)
// ---------------------------------------------------------------------------

fn build_get_request(host: &str, full_path: &str) -> String {
    format!(
        "GET {full_path} HTTP/1.0\r\n\
         Host: {host}\r\n\
         User-Agent: pan/{}\r\n\
         Connection: close\r\n\
         Accept: */*\r\n\
         \r\n",
        env!("CARGO_PKG_VERSION"),
    )
}

/// Find the `\r\n\r\n` header/body boundary in raw bytes.
fn split_http_response(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    for i in 0..bytes.len().saturating_sub(4) {
        if bytes[i..].starts_with(b"\r\n\r\n") {
            return Some((&bytes[..i], &bytes[i + 4..]));
        }
    }
    None
}

/// Extract the `Location` header value from an HTTP response head.
fn parse_location(head: &str) -> Option<&str> {
    for line in head.lines() {
        if line.to_ascii_lowercase().starts_with("location:") {
            return line.split_once(':').map(|x| x.1.trim());
        }
    }
    None
}

enum BytesResult {
    Body(Vec<u8>),
    Redirect(String),
}

/// One-shot HTTP GET returning raw bytes or a redirect target.
async fn get_bytes_once(url: &str, timeout: Duration) -> Result<BytesResult, String> {
    let target = parse_base(url, "")?;
    let request = build_get_request(&target.host, &target.full_path);
    let raw = async_raw_exchange(&target, timeout, request.as_bytes()).await?;
    let (head_bytes, body) =
        split_http_response(&raw).ok_or_else(|| "malformed HTTP response".to_string())?;
    let head_str =
        std::str::from_utf8(head_bytes).map_err(|e| format!("bad response headers: {e}"))?;
    let status_line = head_str.lines().next().unwrap_or("");
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    if (300..400).contains(&code) {
        if let Some(loc) = parse_location(head_str) {
            return Ok(BytesResult::Redirect(loc.to_string()));
        }
        return Err(format!("HTTP {code}: redirect with no Location"));
    }
    if !(200..300).contains(&code) {
        let preview = String::from_utf8_lossy(&body[..body.len().min(200)]);
        return Err(format!("HTTP {code}: {preview}"));
    }
    Ok(BytesResult::Body(body.to_vec()))
}

/// Async raw (bytes) GET exchange, skipping the `String` conversion that would
/// corrupt binary bodies.
async fn async_raw_exchange(
    target: &Target,
    timeout: Duration,
    request: &[u8],
) -> Result<Vec<u8>, String> {
    match &target.scheme {
        Scheme::Http => {
            let mut stream = async_connect(&target.host, target.port, timeout).await?;
            stream
                .write_all(request)
                .await
                .map_err(|e| format!("send: {e}"))?;
            async_read_body(&mut stream, timeout).await
        }
        Scheme::Https => {
            use tokio_rustls::TlsConnector;
            let config = tls_config()?;
            let server_name = rustls::pki_types::ServerName::try_from(target.host.clone())
                .map_err(|_| format!("invalid TLS server name {:?}", target.host))?;
            let connector = TlsConnector::from(config);
            let tcp = async_connect(&target.host, target.port, timeout).await?;
            let mut tls = connector
                .connect(server_name, tcp)
                .await
                .map_err(|e| format!("tls handshake: {e}"))?;
            tls.write_all(request)
                .await
                .map_err(|e| format!("tls send: {e}"))?;
            async_read_body(&mut tls, timeout).await
        }
    }
}

/// GET `url` with redirect following (up to 5 hops) and return the body bytes.
/// Supports `http://` and `https://` URLs.
pub async fn get_bytes_async(url: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let mut url = url.to_string();
    for _ in 0..5 {
        match get_bytes_once(&url, timeout).await? {
            BytesResult::Body(bytes) => return Ok(bytes),
            BytesResult::Redirect(loc) => {
                url = if loc.starts_with('/') {
                    let base = url.split('/').take(3).collect::<Vec<_>>().join("/");
                    format!("{base}{loc}")
                } else {
                    loc
                };
            }
        }
    }
    Err("too many redirects".to_string())
}

/// GET `url` and parse the response body as JSON.
/// Typical use: checking the GitHub API for the latest release.
pub async fn get_json_async(url: &str, timeout: Duration) -> Result<Value, String> {
    let bytes = get_bytes_async(url, timeout).await?;
    serde_json::from_slice(&bytes).map_err(|e| format!("bad response JSON: {e}"))
}

// ---------------------------------------------------------------------------
// Async transport (tokio-based, non-blocking)
// ---------------------------------------------------------------------------

/// Async POST JSON. Same semantics as [`post_json`] but uses tokio I/O so it
/// does not block a worker thread. Callers using it from `async fn` should
/// `.await` it directly.
pub async fn post_json_async(
    base: &str,
    path: &str,
    api_key: Option<&str>,
    body: &Value,
    timeout: Duration,
) -> Result<Value, String> {
    let headers: Vec<(String, String)> = match api_key {
        Some(k) => vec![("Authorization".into(), format!("Bearer {k}"))],
        None => vec![],
    };
    post_json_ex_async(base, path, body, &headers, timeout).await
}

/// Async POST with extra headers. See [`post_json_async`] and [`post_json_ex`].
pub async fn post_json_ex_async(
    base: &str,
    path: &str,
    body: &Value,
    extra_headers: &[(String, String)],
    timeout: Duration,
) -> Result<Value, String> {
    let target = parse_base(base, path)?;
    let mut last_err: Option<(u16, String)> = None;
    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            let delay_ms = 500 * 2u64.pow(attempt - 1);
            sleep(Duration::from_millis(delay_ms + rand_delay(delay_ms))).await;
        }
        let request = build_request_ex(
            &target.host,
            &target.full_path,
            body,
            &extra_headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.clone()))
                .collect::<Vec<_>>(),
        );
        let raw = match target.scheme {
            Scheme::Http => {
                async_http_exchange(&target.host, target.port, timeout, request.as_bytes()).await?
            }
            Scheme::Https => {
                async_https_exchange(&target.host, target.port, timeout, request.as_bytes()).await?
            }
        };
        match parse_response(&raw) {
            Ok(val) => return Ok(val),
            Err((status, msg)) if status >= 429 => {
                let msg_clone = msg.clone();
                last_err = Some((status, msg));
                if let Some(ms) = parse_retry_after(&raw) {
                    sleep(Duration::from_millis(ms)).await;
                }
                if status == 429 || status >= 500 {
                    continue;
                }
                return Err(format!("HTTP {status}: {msg_clone}"));
            }
            Err((_, msg)) => return Err(msg),
        }
    }
    match last_err {
        Some((status, msg)) => Err(format!("HTTP {status} after {MAX_RETRIES} retries: {msg}")),
        None => Err("max retries exceeded".into()),
    }
}

async fn async_connect(host: &str, port: u16, timeout: Duration) -> Result<AsyncTcpStream, String> {
    tokio_timeout(timeout, AsyncTcpStream::connect((host, port)))
        .await
        .map_err(|_| "connect timeout".to_string())?
        .map_err(|e| format!("connect: {e}"))
}

async fn async_read_body(
    stream: &mut (impl AsyncReadExt + Unpin),
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = match tokio_timeout(timeout, stream.read(&mut chunk)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => n,
            Ok(Err(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Ok(Err(e)) => return Err(format!("read: {e}")),
            Err(_) => return Err("read timeout".to_string()),
        };
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_RESPONSE_BYTES {
            break;
        }
    }
    Ok(buf)
}

async fn async_http_exchange(
    host: &str,
    port: u16,
    timeout: Duration,
    request: &[u8],
) -> Result<String, String> {
    let mut stream = async_connect(host, port, timeout).await?;
    stream
        .write_all(request)
        .await
        .map_err(|e| format!("send: {e}"))?;
    let bytes = async_read_body(&mut stream, timeout).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn async_https_exchange(
    host: &str,
    port: u16,
    timeout: Duration,
    request: &[u8],
) -> Result<String, String> {
    use tokio_rustls::TlsConnector;

    let config = tls_config()?;
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| format!("invalid TLS server name {host:?}"))?;
    let connector = TlsConnector::from(config);
    let tcp = async_connect(host, port, timeout).await?;
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("tls handshake: {e}"))?;
    tls.write_all(request)
        .await
        .map_err(|e| format!("tls send: {e}"))?;
    let bytes = async_read_body(&mut tls, timeout).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(scheme: Scheme, host: &str, port: u16, full_path: &str) -> Target {
        Target {
            scheme,
            host: host.into(),
            port,
            full_path: full_path.into(),
        }
    }

    #[test]
    fn parses_http_bases_with_and_without_prefix() {
        assert_eq!(
            parse_base("http://127.0.0.1:11434/v1", "/chat/completions").unwrap(),
            target(Scheme::Http, "127.0.0.1", 11434, "/v1/chat/completions")
        );
        assert_eq!(
            parse_base("http://localhost", "/chat/completions").unwrap(),
            target(Scheme::Http, "localhost", 80, "/chat/completions")
        );
    }

    #[test]
    fn parses_https_and_defaults_to_443() {
        assert_eq!(
            parse_base("https://api.openai.com/v1", "/chat/completions").unwrap(),
            target(Scheme::Https, "api.openai.com", 443, "/v1/chat/completions")
        );
        assert_eq!(
            parse_base("https://example.com:8443", "/x").unwrap(),
            target(Scheme::Https, "example.com", 8443, "/x")
        );
    }

    #[test]
    fn rejects_unknown_scheme() {
        assert!(parse_base("ftp://x/y", "/z").is_err());
        assert!(parse_base("api.openai.com", "/z").is_err());
    }

    #[test]
    fn tls_config_builds() {
        // Exercises the ring provider + root store wiring (no network).
        assert!(tls_config().is_ok());
    }
}
