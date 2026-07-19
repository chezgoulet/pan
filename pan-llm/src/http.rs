//! # A tiny, std-only, blocking HTTP/1.0 JSON client.
//!
//! Deliberately minimal — no TLS, no async, no dependencies. It targets **local,
//! plain-HTTP** OpenAI-compatible servers (Ollama, llama.cpp, LM Studio) and the
//! mock server the tests spin up. HTTP/1.0 means the server neither keeps the
//! connection alive nor chunk-encodes the body: read to EOF, split head from
//! body, done — the whole client is ~a page of honest code.
//!
//! Cloud BYOK over TLS (`https://`) is an additive transport behind the same
//! `post_json` shape; today an `https://` base is a clear, early error rather
//! than a silent plaintext downgrade. This mirrors `pan-daemon`'s `llm.rs`
//! client; the tool-use *mapping* on top is what this crate adds.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use pan_core::schema::Value;

const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

/// POST `body` as JSON to `base` + `path` and parse the JSON response. `api_key`,
/// if present, is sent as a bearer token. Any transport/status/parse failure is a
/// human-readable `Err(String)` — the provider turns that into `Conclude(Abandoned)`.
pub fn post_json(
    base: &str,
    path: &str,
    api_key: Option<&str>,
    body: &Value,
    timeout: Duration,
) -> Result<Value, String> {
    let (host, port, full_path) = parse_base(base, path)?;

    let mut stream =
        TcpStream::connect((host.as_str(), port)).map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;

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

    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("send: {e}"))?;

    let mut raw = String::new();
    stream
        .take(MAX_RESPONSE_BYTES)
        .read_to_string(&mut raw)
        .map_err(|e| format!("read: {e}"))?;

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

/// Split `http://host[:port][/prefix]` + a leading-slash `path` into
/// `(host, port, full_path)`. `https://` is a deliberate error until the TLS
/// transport lands.
fn parse_base(base: &str, path: &str) -> Result<(String, u16, String), String> {
    if base.starts_with("https://") {
        return Err(
            "pan-llm targets plain-http endpoints; https (TLS) transport is not yet implemented"
                .to_string(),
        );
    }
    let rest = base
        .strip_prefix("http://")
        .ok_or_else(|| format!("base {base:?} must start with http://"))?;
    let (authority, prefix) = match rest.split_once('/') {
        Some((a, p)) => (a, p.trim_end_matches('/')),
        None => (rest, ""),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| format!("bad port {p:?}"))?),
        None => (authority, 80),
    };
    if host.is_empty() {
        return Err("empty host".into());
    }
    let full_path = if prefix.is_empty() {
        path.to_string()
    } else {
        format!("/{prefix}{path}")
    };
    Ok((host.to_string(), port, full_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bases_with_and_without_prefix() {
        assert_eq!(
            parse_base("http://127.0.0.1:11434/v1", "/chat/completions").unwrap(),
            ("127.0.0.1".into(), 11434, "/v1/chat/completions".into())
        );
        assert_eq!(
            parse_base("http://localhost", "/chat/completions").unwrap(),
            ("localhost".into(), 80, "/chat/completions".into())
        );
        assert_eq!(
            parse_base("http://host:8080/", "/x").unwrap(),
            ("host".into(), 8080, "/x".into())
        );
    }

    #[test]
    fn https_is_a_clear_error_for_now() {
        assert!(parse_base("https://api.openai.com/v1", "/chat/completions")
            .unwrap_err()
            .contains("TLS"));
    }
}
