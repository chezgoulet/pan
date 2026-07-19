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

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// POST `body` as JSON to `base` + `path` and parse the JSON response. `api_key`,
/// if present, is sent as a bearer token. `http://` and `https://` bases are both
/// supported. Any transport/status/parse failure is a human-readable `Err(String)`
/// — the provider turns that into `Conclude(Abandoned)`.
pub fn post_json(
    base: &str,
    path: &str,
    api_key: Option<&str>,
    body: &Value,
    timeout: Duration,
) -> Result<Value, String> {
    let target = parse_base(base, path)?;
    let request = build_request(&target.host, &target.full_path, api_key, body);
    let raw = match target.scheme {
        Scheme::Http => http_exchange(&target.host, target.port, timeout, request.as_bytes())?,
        Scheme::Https => https_exchange(&target.host, target.port, timeout, request.as_bytes())?,
    };
    parse_response(&raw)
}

// ---------------------------------------------------------------------------
// Request / response (transport-independent)
// ---------------------------------------------------------------------------

fn build_request(host: &str, full_path: &str, api_key: Option<&str>, body: &Value) -> String {
    let payload = body.to_string();
    let mut request = format!(
        "POST {full_path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        payload.len()
    );
    if let Some(key) = api_key {
        request.push_str(&format!("Authorization: Bearer {key}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&payload);
    request
}

fn parse_response(raw: &str) -> Result<Value, String> {
    let (head, response_body) = raw
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response".to_string())?;
    let status_line = head.lines().next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("bad status line: {status_line:?}"))?;
    if status != 200 {
        return Err(format!(
            "HTTP {status}: {}",
            &response_body[..response_body.len().min(400)]
        ));
    }
    serde_json::from_str(response_body).map_err(|e| format!("bad response JSON: {e}"))
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
