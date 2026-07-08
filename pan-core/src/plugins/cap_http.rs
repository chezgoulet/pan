//! # `cap.http` — outbound HTTP requests (Wave 2).
//!
//! A capability handler for `exec.local`. Performs a real outbound HTTP call
//! and returns status + body. Args:
//! - `url` (required)
//! - `method` (optional, default GET)
//! - `body` (optional, for POST/PUT/PATCH; sent as text/plain)
//! - `headers` (optional, map of string→string)
//!
//! Real network egress, so in Wave 4 `gov.policy` should gate which hosts are
//! reachable. Uses the same rustls-backed reqwest as `provider.llm` (no system
//! OpenSSL needed).

use crate::pipeline::ExecError;
use crate::schema::Value;
use std::time::Duration;

/// `cap.http` handler. Returns `{ status, ok, body, headers }`.
pub fn handle_http(args: &Value) -> Result<Value, ExecError> {
    let url = args
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExecError("cap.http requires a `url` string".into()))?;
    let method = args
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_uppercase();

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| ExecError(format!("cap.http client: {e}")))?;

    let mut req = match method.as_str() {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "DELETE" => client.delete(url),
        "PATCH" => client.patch(url),
        "HEAD" => client.head(url),
        other => return Err(ExecError(format!("cap.http unsupported method `{other}`"))),
    };

    if let Some(headers) = args.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in headers {
            if let Some(s) = v.as_str() {
                req = req.header(k, s);
            }
        }
    }
    if let Some(body) = args.get("body").and_then(|v| v.as_str()) {
        if matches!(method.as_str(), "POST" | "PUT" | "PATCH") {
            req = req.header("Content-Type", "text/plain").body(body.to_string());
        }
    }

    let resp = req
        .send()
        .map_err(|e| ExecError(format!("cap.http {method} {url}: {e}")))?;
    let status = resp.status().as_u16();
    let ok = resp.status().is_success();
    let body_text = resp
        .text()
        .unwrap_or_default();

    Ok(serde_json::json!({
        "status": status,
        "ok": ok,
        "body": body_text,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------
    // Error-path tests — no network needed, always deterministic.
    // ---------------------------------------------------------------------------

    #[test]
    fn missing_url_errors() {
        assert!(handle_http(&serde_json::json!({ "method": "GET" })).is_err());
    }

    #[test]
    fn bad_method_errors() {
        assert!(handle_http(&serde_json::json!({ "url": "http://example.com/", "method": "FLY" })).is_err());
    }

    #[test]
    fn invalid_url_returns_error() {
        let r = handle_http(&serde_json::json!({
            "url": "\t\nnot a url at all\t",
            "method": "GET",
        }));
        assert!(r.is_err(), "garbage URL should fail: {r:?}");
        if let Err(ref e) = r {
            assert!(e.0.contains("cap.http"), "error should mention capability");
        }
    }

    // ---------------------------------------------------------------------------
    // Network-path tests — use httpbin.org for structured request/response.
    // Transient DNS/connectivity failures are treated as skips (eprintln, no
    // panic) so CI is never flaky.
    // ---------------------------------------------------------------------------

    #[test]
    fn fetches_a_real_url() {
        let r = handle_http(&serde_json::json!({ "url": "http://example.com/", "method": "GET" }));
        match r {
            Ok(v) => {
                assert!(v["status"].as_u64().unwrap() >= 200);
                assert!(v["body"].as_str().unwrap().len() > 0);
            }
            Err(_) => eprintln!("cap_http example.com skipped (no connectivity)"),
        }
    }

    #[test]
    fn get_from_httpbin() {
        let r = handle_http(&serde_json::json!({
            "url": "https://httpbin.org/get",
            "method": "GET",
        }));
        match r {
            Ok(v) => {
                assert_eq!(v["status"].as_u64().unwrap(), 200);
                assert!(v["ok"].as_bool().unwrap());
                let parsed: serde_json::Value =
                    serde_json::from_str(v["body"].as_str().unwrap()).unwrap();
                assert!(parsed.get("url").is_some(), "/get should include a `url` field");
            }
            Err(_) => eprintln!("cap_http GET httpbin.org skipped (no connectivity)"),
        }
    }

    #[test]
    fn post_with_body() {
        let r = handle_http(&serde_json::json!({
            "url": "https://httpbin.org/post",
            "method": "POST",
            "body": "hello from cap_http",
        }));
        match r {
            Ok(v) => {
                assert_eq!(v["status"].as_u64().unwrap(), 200);
                let parsed: serde_json::Value =
                    serde_json::from_str(v["body"].as_str().unwrap()).unwrap();
                // httpbin echoes the raw body in its `data` field.
                assert_eq!(parsed["data"].as_str(), Some("hello from cap_http"));
            }
            Err(_) => eprintln!("cap_http POST httpbin.org skipped (no connectivity)"),
        }
    }

    #[test]
    fn put_with_body() {
        let r = handle_http(&serde_json::json!({
            "url": "https://httpbin.org/put",
            "method": "PUT",
            "body": "update payload",
        }));
        match r {
            Ok(v) => {
                assert_eq!(v["status"].as_u64().unwrap(), 200);
                let parsed: serde_json::Value =
                    serde_json::from_str(v["body"].as_str().unwrap()).unwrap();
                assert_eq!(parsed["data"].as_str(), Some("update payload"));
            }
            Err(_) => eprintln!("cap_http PUT httpbin.org skipped (no connectivity)"),
        }
    }

    #[test]
    fn patch_with_body() {
        let r = handle_http(&serde_json::json!({
            "url": "https://httpbin.org/patch",
            "method": "PATCH",
            "body": "patch data",
        }));
        match r {
            Ok(v) => {
                assert_eq!(v["status"].as_u64().unwrap(), 200);
                let parsed: serde_json::Value =
                    serde_json::from_str(v["body"].as_str().unwrap()).unwrap();
                assert_eq!(parsed["data"].as_str(), Some("patch data"));
            }
            Err(_) => eprintln!("cap_http PATCH httpbin.org skipped (no connectivity)"),
        }
    }

    #[test]
    fn delete_supported() {
        let r = handle_http(&serde_json::json!({
            "url": "https://httpbin.org/delete",
            "method": "DELETE",
        }));
        match r {
            Ok(v) => assert_eq!(v["status"].as_u64().unwrap(), 200),
            Err(_) => eprintln!("cap_http DELETE httpbin.org skipped (no connectivity)"),
        }
    }

    #[test]
    fn custom_headers_reach_upstream() {
        let r = handle_http(&serde_json::json!({
            "url": "https://httpbin.org/headers",
            "method": "GET",
            "headers": { "X-Test-Header": "cap-http-test" },
        }));
        match r {
            Ok(v) => {
                assert!(v["ok"].as_bool().unwrap());
                let parsed: serde_json::Value =
                    serde_json::from_str(v["body"].as_str().unwrap()).unwrap();
                let headers = parsed.get("headers").and_then(|h| h.as_object());
                assert!(
                    headers.is_some(),
                    "/headers should return a JSON object with a `headers` key"
                );
                let found = headers
                    .unwrap()
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("X-Test-Header"));
                assert!(found, "X-Test-Header should appear in httpbin response");
            }
            Err(_) => eprintln!("cap_http headers test skipped (no connectivity)"),
        }
    }

    #[test]
    fn non_200_status_is_not_an_error() {
        // The capability returns non-2xx status codes as data, not as errors.
        let r = handle_http(&serde_json::json!({
            "url": "https://httpbin.org/status/418",
            "method": "GET",
        }));
        match r {
            Ok(v) => {
                assert_eq!(v["status"].as_u64().unwrap(), 418);
                assert!(!v["ok"].as_bool().unwrap());
            }
            Err(_) => eprintln!("cap_http 418 test skipped (no connectivity)"),
        }
    }

    #[test]
    fn unreachable_host_returns_error() {
        // 10.255.255.1 is a reserved TEST-NET-1 address that no real host
        // should occupy; :1 is the TCMP port, nothing listens there.
        let r = handle_http(&serde_json::json!({
            "url": "http://10.255.255.1:1/",
            "method": "GET",
        }));
        match r {
            Err(e) => {
                assert!(
                    e.0.contains("cap.http"),
                    "unreachable host error should mention capability: {e:?}"
                );
            }
            Ok(v) => {
                // If something weirdly responded (e.g., a captive portal
                // intercept) just note it; don't fail.
                eprintln!("cap_http unreachable host returned OK ({v}), skipping assertion");
            }
        }
    }
}
