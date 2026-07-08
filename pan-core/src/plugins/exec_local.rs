//! # `exec.local` — in-process execution (Wave 1).
//!
//! The `execute` stage plugin. Runs `cap.shell` by shelling out to the OS. This
//! is the trivial, *unsafe-by-design* executor the manifest calls the walking
//! skeleton: it performs effects on the host with no sandboxing. Wave 4 replaces
//! it with `exec.docker`/`exec.ssh` for anything touching real tools; until then
//! it is gated only by the `govern` stage (Wave 1 = `gov.allow`, which allows
//! everything). That asymmetry is documented, not hidden: host execution without
//! a real govern policy is the whole point of doing governance *before* chat
//! exposure (manifest Wave 4 note).
//!
//! Design: the executor is capability-agnostic. A capability's args are handed to
//! a registered handler keyed by capability id. `cap.shell` is the built-in one;
//! more can be registered by the host. This keeps the executor from knowing
//! about every verb (the core's "plugins plug in" boundary).

use crate::pipeline::{ExecError, Executor};
use crate::schema::Value;
use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;

/// A handler turns capability args into a JSON result value.
pub type Handler = Box<dyn Fn(&Value) -> Result<Value, ExecError> + Send + Sync>;

pub struct LocalExecutor {
    handlers: Mutex<HashMap<String, Handler>>,
}

impl Default for LocalExecutor {
    fn default() -> Self {
        let mut handlers: HashMap<String, Handler> = HashMap::new();
        handlers.insert("cap.shell".to_string(), Box::new(run_shell));
        Self {
            handlers: Mutex::new(handlers),
        }
    }
}

impl LocalExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a handler for a capability id (overrides built-ins).
    pub fn register<F>(&self, capability: &str, handler: F)
    where
        F: Fn(&Value) -> Result<Value, ExecError> + 'static + Send + Sync,
    {
        self.handlers
            .lock()
            .unwrap()
            .insert(capability.to_string(), Box::new(handler));
    }
}

/// `cap.shell`: args `{ "command": "..." }` (optional `cwd`). Runs via `sh -c`.
pub fn run_shell(args: &Value) -> Result<Value, ExecError> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExecError("cap.shell requires a `command` string arg".into()))?;
    let cwd = args.get("cwd").and_then(|v| v.as_str());

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    match cmd.output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            Ok(serde_json::json!({
                "exit_code": out.status.code(),
                "stdout": stdout,
                "stderr": stderr,
                "success": out.status.success(),
            }))
        }
        Err(e) => Err(ExecError(format!("failed to spawn cap.shell: {e}"))),
    }
}

impl Executor for LocalExecutor {
    fn id(&self) -> &str {
        "exec.local"
    }

    fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError> {
        let handlers = self.handlers.lock().unwrap();
        match handlers.get(capability) {
            Some(h) => h(args),
            None => Err(ExecError(format!(
                "exec.local has no handler for capability `{capability}`"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_runs_and_captures_stdout() {
        let ex = LocalExecutor::new();
        let r = ex
            .execute(
                "cap.shell",
                &serde_json::json!({"command": "echo hello-pan"}),
            )
            .unwrap();
        assert_eq!(r["stdout"].as_str().unwrap().trim(), "hello-pan");
        assert_eq!(r["success"], true);
        assert_eq!(r["exit_code"], 0);
    }

    #[test]
    fn shell_reports_nonzero_exit() {
        let ex = LocalExecutor::new();
        let r = ex
            .execute("cap.shell", &serde_json::json!({"command": "exit 3"}))
            .unwrap();
        assert_eq!(r["exit_code"], 3);
        assert_eq!(r["success"], false);
    }

    #[test]
    fn missing_command_arg_errors() {
        let ex = LocalExecutor::new();
        let err = ex.execute("cap.shell", &serde_json::json!({})).unwrap_err();
        assert!(err.0.contains("command"));
    }

    #[test]
    fn unknown_capability_errors() {
        let ex = LocalExecutor::new();
        let err = ex.execute("cap.ghost", &Value::Null).unwrap_err();
        assert!(err.0.contains("no handler"));
    }

    #[test]
    fn custom_handler_registers() {
        let ex = LocalExecutor::new();
        ex.register("cap.echo", |args| {
            Ok(serde_json::json!({"got": args}))
        });
        let r = ex.execute("cap.echo", &serde_json::json!({"x": 1})).unwrap();
        assert_eq!(r["got"]["x"], 1);
    }
}
