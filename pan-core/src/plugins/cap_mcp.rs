//! # `cap.mcp` — bridge to any MCP (Model Context Protocol) server (Wave 2).
//!
//! The single highest-leverage plugin in the manifest: one plugin, and every
//! tool an MCP server exposes becomes an invocable Pan capability. We support
//! the **stdio** transport first (the common case for local tool servers):
//! spawn the server, run the JSON-RPC initialize handshake, enumerate its
//! tools, and register each as `cap.mcp.<tool_name>` with a handler that calls
//! `tools/call` and returns the result.
//!
//! Design notes:
//! - The server runs as a long-lived child process; stdin/stdout carry
//!   newline-delimited JSON-RPC. Stderr is inherited (servers log there).
//! - We key tool results on the JSON-RPC `id` to match responses to requests.
//! - Errors from the server are surfaced as `ExecError` so the pipeline records
//!   a failed effect rather than panicking the agent.

use crate::pipeline::ExecError;
use crate::schema::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A live stdio MCP session.
pub struct McpSession {
    child: Mutex<Child>,
    stdin: Mutex<std::process::ChildStdin>,
    // Responses are read off the child's stdout by a dedicated reader thread
    // and parked here keyed by request id. Shared via Arc so the reader and the
    // caller observe the SAME map.
    pending: Arc<Mutex<HashMap<u64, Value>>>,
    next_id: AtomicU64,
}

impl McpSession {
    /// Spawn `command` (e.g. `npx -y @modelcontextprotocol/server-everything`)
    /// and complete the initialize handshake. Returns the session with tools
    /// enumerated (call [`tools`](Self::tools) after).
    pub fn spawn(command: &str, args: &[&str]) -> Result<Self, ExecError> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| ExecError(format!("mcp spawn {command}: {e}")))?;

        let stdin = child.stdin.take().ok_or_else(|| ExecError("mcp: no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| ExecError("mcp: no stdout".into()))?;

        let pending: Arc<Mutex<HashMap<u64, Value>>> = Arc::new(Mutex::new(HashMap::new()));

        let session = McpSession {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            pending: Arc::clone(&pending),
            next_id: AtomicU64::new(1),
        };

        // Reader thread: parse newline-delimited JSON-RPC responses, park them
        // by id. Notification shapes (no id) are ignored for call-matching.
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
                        pending.lock().unwrap().insert(id, v);
                    }
                }
            }
        });

        // Handshake: initialize → initialized notification → tools/list.
        session.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "pan", "version": "0.1.0"}
            }),
        )?;
        session.notify("notifications/initialized", serde_json::json!({}))?;
        Ok(session)
    }

    /// List tool definitions from the server. Each becomes a capability.
    pub fn tools(&self) -> Result<Vec<McpTool>, ExecError> {
        let resp = self.request("tools/list", serde_json::json!({}))?;
        let arr = resp
            .get("result")
            .and_then(|r| r.get("tools"))
            .and_then(|t| t.as_array())
            .ok_or_else(|| ExecError("mcp tools/list: no tools array".into()))?;
        Ok(arr
            .iter()
            .filter_map(|t| {
                Some(McpTool {
                    name: t.get("name")?.as_str()?.to_string(),
                    description: t.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string(),
                    input_schema: t.get("inputSchema").cloned().unwrap_or(Value::Null),
                })
            })
            .collect())
    }

    /// Call a tool by name with arbitrary args. Returns the tool's content.
    pub fn call(&self, name: &str, args: &Value) -> Result<Value, ExecError> {
        let resp = self.request(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": args }),
        )?;
        if let Some(err) = resp.get("error") {
            return Err(ExecError(format!("mcp tools/call {name}: {err}")));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    // --- JSON-RPC plumbing ------------------------------------------------

    fn notify(&self, method: &str, params: Value) -> Result<(), ExecError> {
        let msg = serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.send(&msg)
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, ExecError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        });
        self.send(&msg)?;
        // Block until the reader thread parks a response with this id.
        for _ in 0..200 {
            if let Some(v) = self.pending.lock().unwrap().remove(&id) {
                return Ok(v);
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        Err(ExecError(format!("mcp: no response for {method} (id {id})")))
    }

    fn send(&self, msg: &Value) -> Result<(), ExecError> {
        let mut stdin = self.stdin.lock().unwrap();
        let line = serde_json::to_string(msg).map_err(|e| ExecError(format!("mcp serialize: {e}")))?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|e| ExecError(format!("mcp write: {e}")))?;
        Ok(())
    }
}

/// One MCP tool, mirrored as a Pan capability.
#[derive(Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl Drop for McpSession {
    fn drop(&mut self) {
        let _ = self.child.lock().unwrap().kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal stdio MCP server: handles initialize + tools/list + tools/call
    /// for one tool `echo` that returns its input. Written to a temp file and
    /// spawned by the test so we exercise the real spawn/handshake/call path.
    const SERVER_PY: &str = r#"
import sys, json
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()
def main():
    for line in sys.stdin:
        m = json.loads(line)
        if m.get("method") == "initialize":
            send({"jsonrpc":"2.0","id":m["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"test","version":"0"}}})
        elif m.get("method") == "notifications/initialized":
            pass
        elif m.get("method") == "tools/list":
            send({"jsonrpc":"2.0","id":m["id"],"result":{"tools":[{"name":"echo","description":"echo input","inputSchema":{"type":"object","properties":{"msg":{"type":"string"}}}}]}})
        elif m.get("method") == "tools/call":
            args = m["params"].get("arguments", {})
            send({"jsonrpc":"2.0","id":m["id"],"result":{"content":[{"type":"text","text":"got: " + str(args.get("msg","",))}]}})
main()
"#;

    fn write_server() -> String {
        let p = std::env::temp_dir().join(format!("pan_mcp_server_{}.py", std::process::id()));
        std::fs::write(&p, SERVER_PY).unwrap();
        p.to_string_lossy().to_string()
    }

    #[test]
    fn spawn_enumerate_and_call() {
        let server = write_server();
        let session = McpSession::spawn("python3", &[&server]).expect("spawn mcp server");
        let tools = session.tools().expect("list tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");

        let result = session
            .call("echo", &serde_json::json!({ "msg": "hi" }))
            .expect("call echo");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("hi"), "unexpected: {text}");
        let _ = std::fs::remove_file(&server);
    }
}

