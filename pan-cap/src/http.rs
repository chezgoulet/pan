//! # `cap.http` — governed web access.
//!
//! Provides `cap.http.get` and `cap.http.post`, returning status + body. The
//! governor decides *whether* a persona may reach `cap.http` at all; arg-level
//! policy (e.g. an allowlisted host set) lives in the governor, not here.
//!
//! Transport is blocking `std::net::TcpStream` over plain HTTP (no TLS), matching
//! the pattern of `pan-llm::http` — fine for local/trusted-network use.
//! HTTPS support is a later refinement (lift the rustls client from pan-llm).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use pan_core::pipeline::ExecError;
use pan_core::schema::{Capability, Value};
use pan_core::toolbox::CapabilityProvider;

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY: usize = 8 * 1024 * 1024;

/// HTTP capability: governed GET/POST requests to external URLs.
pub struct HttpCaps;

impl Default for HttpCaps {
    fn default() -> Self {
        Self
    }
}

impl HttpCaps {
    pub fn new() -> Self {
        Self
    }

    fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ExecError> {
        args.get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExecError(format!("`{key}` must be a string")))
    }

    fn build_request(
        method: &str,
        host: &str,
        path: &str,
        headers: &Value,
        body: Option<&str>,
    ) -> String {
        let mut req = format!("{method} {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n");
        if let Some(headers_obj) = headers.as_object() {
            for (k, v) in headers_obj {
                if let Some(val) = v.as_str() {
                    req.push_str(&format!("{k}: {val}\r\n"));
                }
            }
        }
        if let Some(b) = body {
            req.push_str(&format!("Content-Length: {}\r\n\r\n{}", b.len(), b));
        } else {
            req.push_str("\r\n");
        }
        req
    }

    fn exchange(host: &str, port: u16, request: &str) -> Result<(u16, String), String> {
        let mut stream = TcpStream::connect((host, port)).map_err(|e| format!("connect: {e}"))?;
        stream
            .set_read_timeout(Some(TIMEOUT))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(TIMEOUT))
            .map_err(|e| e.to_string())?;
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("send: {e}"))?;

        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.len() > MAX_BODY {
                        break;
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(format!("read: {e}")),
            }
        }
        let raw = String::from_utf8_lossy(&buf).into_owned();
        let (head, response_body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let status_line = head.lines().next().unwrap_or("");
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| format!("bad status line: {status_line:?}"))?;
        Ok((status, response_body.to_string()))
    }

    fn do_get(url: &str, headers: &Value) -> Result<Value, ExecError> {
        let parsed = parse_url(url)?;
        let request = Self::build_request("GET", &parsed.host, &parsed.path, headers, None);
        let (status, body) =
            Self::exchange(&parsed.host, parsed.port, &request).map_err(ExecError)?;
        Ok(serde_json::json!({ "status": status, "body": body }))
    }

    fn do_post(url: &str, headers: &Value, body: &str) -> Result<Value, ExecError> {
        let parsed = parse_url(url)?;
        let request = Self::build_request("POST", &parsed.host, &parsed.path, headers, Some(body));
        let (status, body) =
            Self::exchange(&parsed.host, parsed.port, &request).map_err(ExecError)?;
        Ok(serde_json::json!({ "status": status, "body": body }))
    }
}

#[async_trait::async_trait]
impl CapabilityProvider for HttpCaps {
    fn id(&self) -> &str {
        "cap.http"
    }

    fn capabilities(&self) -> Vec<Capability> {
        let url_with_headers = serde_json::json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": { "type": "string", "description": "the HTTP URL to request" },
                "headers": {
                    "type": "object",
                    "description": "optional additional headers as key/value pairs",
                    "additionalProperties": { "type": "string" }
                }
            }
        });
        vec![
            Capability {
                id: "cap.http.get".into(),
                summary: "fetch a URL via HTTP GET, returning status + body".into(),
                args_schema: url_with_headers.clone(),
            },
            Capability {
                id: "cap.http.post".into(),
                summary: "POST a string body to a URL, returning status + body".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "required": ["url", "body"],
                    "properties": {
                        "url": { "type": "string" },
                        "body": { "type": "string" },
                        "headers": {
                            "type": "object",
                            "additionalProperties": { "type": "string" }
                        }
                    }
                }),
            },
        ]
    }

    async fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError> {
        let url = Self::arg_str(args, "url")?;
        let headers = args.get("headers").cloned().unwrap_or(Value::Null);
        match capability {
            "cap.http.get" => Self::do_get(url, &headers),
            "cap.http.post" => {
                let body = Self::arg_str(args, "body")?;
                Self::do_post(url, &headers, body)
            }
            other => Err(ExecError(format!("cap.http has no `{other}`"))),
        }
    }
}

struct ParsedUrl {
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> Result<ParsedUrl, ExecError> {
    let (rest, default_port) = if let Some(r) = url.strip_prefix("http://") {
        (r, 80u16)
    } else {
        return Err(ExecError(format!(
            "unsupported URL scheme (only http:// is supported): {url:?}"
        )));
    };

    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| ExecError(format!("bad port {p:?}")))?,
        ),
        None => (authority.to_string(), default_port),
    };
    if host.is_empty() {
        return Err(ExecError("empty host in URL".into()));
    }
    Ok(ParsedUrl { host, port, path })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_urls() {
        let p = parse_url("http://example.com").unwrap();
        assert_eq!(p.host, "example.com");
        assert_eq!(p.port, 80);
        assert_eq!(p.path, "/");

        let p = parse_url("http://127.0.0.1:8080/foo/bar").unwrap();
        assert_eq!(p.host, "127.0.0.1");
        assert_eq!(p.port, 8080);
        assert_eq!(p.path, "/foo/bar");
    }

    #[test]
    fn rejects_https_and_other_schemes() {
        assert!(parse_url("https://example.com").is_err());
        assert!(parse_url("ftp://x.y").is_err());
        assert!(parse_url("example.com").is_err());
    }

    #[test]
    fn capability_schema_is_valid() {
        let caps = HttpCaps::new().capabilities();
        assert_eq!(caps.len(), 2);
        assert!(caps.iter().any(|c| c.id == "cap.http.get"));
        assert!(caps.iter().any(|c| c.id == "cap.http.post"));
    }
}
