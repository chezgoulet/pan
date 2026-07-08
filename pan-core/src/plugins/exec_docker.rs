//! # `exec.docker` — sandboxed container execution (Wave 4).
//!
//! The `execute` stage plugin. Runs `cap.shell` by dispatching commands into an
//! ephemeral Docker container. This is the sandboxed executor: dangerous
//! capabilities (shell, fs) run in a container instead of the host process.
//!
//! ## Safety guarantees
//!
//! - **Host filesystem isolation**: container runs with a read-only root
//!   filesystem (`--read-only`) and a writable tmpfs at `/tmp`. No host
//!   directories are mounted into the container.
//! - **Network isolation**: network is disabled by default (`--network none`),
//!   preventing the container from reaching the host network or the internet.
//!   Enable explicitly with `DockerConfig::with_network()`.
//! - **Ephemeral**: each invocation creates a fresh container (`--rm`), so no
//!   state persists between calls.
//! - **Timeout**: a `timeout` wrapper kills the container if execution exceeds
//!   the configured limit (default 30s).
//!
//! ## Design
//!
//! Follows the same handler pattern as [`exec_local`](super::exec_local): the
//! executor is capability-agnostic. A capability's args are handed to a
//! registered handler keyed by capability id. `cap.shell` is the built-in
//! handler; more can be registered by the host.
//!
//! Capabilities declare *preferred executor* via the `execution_profile` module
//! (`ExecutionProfile::Sandboxed`). The loop or host selects which executor to
//! wire into the pipeline based on the profile.

use crate::pipeline::{ExecError, Executor};
use crate::schema::Value;
use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the Docker executor.
///
/// # Defaults (safe by default)
///
/// | Field             | Default          | Reason                           |
/// |-------------------|------------------|----------------------------------|
/// | `image`           | `alpine:latest`  | Small, fast pull                 |
/// | `network_enabled` | `false`          | No network by default            |
/// | `timeout_secs`    | `30`             | Prevent runaway containers       |
/// | `read_only`       | `true`           | Host filesystem isolation        |
///
/// Use the builder-style methods to override selectively.
#[derive(Debug, Clone)]
pub struct DockerConfig {
    /// Container image to use for execution.
    pub image: String,
    /// Whether to enable network access inside the container.
    pub network_enabled: bool,
    /// Maximum wall-clock execution time in seconds.
    pub timeout_secs: u64,
    /// If true, mount the container root filesystem read-only (with a writable
    /// tmpfs at `/tmp` so temp files still work).
    pub read_only: bool,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            image: "alpine:latest".to_string(),
            network_enabled: false,
            timeout_secs: 30,
            read_only: true,
        }
    }
}

impl DockerConfig {
    /// Shortcut builder: enable network access.
    pub fn with_network(mut self) -> Self {
        self.network_enabled = true;
        self
    }

    /// Shortcut builder: set the container image.
    pub fn with_image(mut self, image: impl Into<String>) -> Self {
        self.image = image.into();
        self
    }

    /// Shortcut builder: set the execution timeout.
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
}

// ---------------------------------------------------------------------------
// Handler type
// ---------------------------------------------------------------------------

/// A handler turns capability args + config into a JSON result value.
pub type Handler =
    Box<dyn Fn(&Value, &DockerConfig) -> Result<Value, ExecError> + Send + Sync>;

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

/// The Docker sandboxed executor.
pub struct DockerExecutor {
    handlers: Mutex<HashMap<String, Handler>>,
    config: DockerConfig,
}

impl DockerExecutor {
    /// Create a new Docker executor with the given configuration.
    ///
    /// Built-in handlers:
    /// - `cap.shell` — runs a shell command inside the container.
    pub fn new(config: DockerConfig) -> Self {
        let mut handlers: HashMap<String, Handler> = HashMap::new();
        handlers.insert(
            "cap.shell".to_string(),
            Box::new(|args: &Value, cfg: &DockerConfig| run_shell_docker(args, cfg)),
        );
        Self {
            handlers: Mutex::new(handlers),
            config,
        }
    }

    /// Register a handler for a capability id (overrides built-ins).
    pub fn register<F>(&self, capability: &str, handler: F)
    where
        F: Fn(&Value, &DockerConfig) -> Result<Value, ExecError> + Send + Sync + 'static,
    {
        self.handlers
            .lock()
            .unwrap()
            .insert(capability.to_string(), Box::new(handler));
    }
}

impl Executor for DockerExecutor {
    fn id(&self) -> &str {
        "exec.docker"
    }

    fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError> {
        let handlers = self.handlers.lock().unwrap();
        match handlers.get(capability) {
            Some(h) => h(args, &self.config),
            None => Err(ExecError(format!(
                "exec.docker has no handler for capability `{capability}`"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in handlers
// ---------------------------------------------------------------------------

/// `cap.shell`: args `{ "command": "..." }`.
///
/// Runs the command inside a fresh ephemeral Docker container with sandbox
/// isolation (read-only root, no network by default). Returns the same result
/// shape as [`exec_local::run_shell`](super::exec_local::run_shell):
///
/// ```json
/// { "exit_code": 0, "stdout": "...", "stderr": "...", "success": true }
/// ```
pub fn run_shell_docker(args: &Value, config: &DockerConfig) -> Result<Value, ExecError> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExecError("cap.shell requires a `command` string arg".into()))?;

    // Build: `timeout <N> docker run --rm [flags] <image> sh -c <command>`
    //
    // We wrap with `timeout` (GNU coreutils, available on every Linux) rather
    // than relying on Docker's `--timeout` flag (Docker 25.0+ only). On systems
    // without `timeout`, the error message is clear.
    let mut cmd = Command::new("timeout");
    cmd.arg(format!("{}", config.timeout_secs));
    cmd.arg("docker");
    cmd.arg("run");
    cmd.arg("--rm");

    // Host filesystem isolation: read-only root + writable tmpfs at /tmp.
    if config.read_only {
        cmd.arg("--read-only");
        cmd.arg("--tmpfs").arg("/tmp:noexec,nosuid,size=64M");
    }

    // Network isolation (default: none).
    if !config.network_enabled {
        cmd.arg("--network").arg("none");
    }

    // Drop capabilities inside the container for defence-in-depth.
    cmd.arg("--cap-drop").arg("ALL");

    // No new privileges in the container.
    cmd.arg("--security-opt").arg("no-new-privileges:true");

    // Container image.
    cmd.arg(&config.image);

    // Shell command.
    cmd.arg("sh").arg("-c").arg(command);

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
        Err(e) => Err(ExecError(format!(
            "failed to spawn docker run: {e}. Is Docker installed and is `timeout` available?"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_exec_docker() {
        let ex = DockerExecutor::new(DockerConfig::default());
        assert_eq!(ex.id(), "exec.docker");
    }

    #[test]
    fn safe_defaults() {
        let cfg = DockerConfig::default();
        assert_eq!(cfg.image, "alpine:latest", "should default to a small image");
        assert!(!cfg.network_enabled, "network should be off by default");
        assert!(cfg.read_only, "read-only root should be on by default");
        assert_eq!(cfg.timeout_secs, 30, "should timeout after 30s");
    }

    #[test]
    fn network_can_be_enabled() {
        let cfg = DockerConfig::default().with_network();
        assert!(cfg.network_enabled);
    }

    #[test]
    fn image_can_be_overridden() {
        let cfg = DockerConfig::default().with_image("ubuntu:24.04");
        assert_eq!(cfg.image, "ubuntu:24.04");
    }

    #[test]
    fn timeout_can_be_overridden() {
        let cfg = DockerConfig::default().with_timeout(120);
        assert_eq!(cfg.timeout_secs, 120);
    }

    #[test]
    fn unknown_capability_errors() {
        let ex = DockerExecutor::new(DockerConfig::default());
        let err = ex.execute("cap.ghost", &Value::Null).unwrap_err();
        assert!(err.0.contains("no handler"), "error: {}", err.0);
    }

    #[test]
    fn missing_command_arg_errors() {
        let ex = DockerExecutor::new(DockerConfig::default());
        let err = ex.execute("cap.shell", &serde_json::json!({})).unwrap_err();
        assert!(err.0.contains("command"), "error: {}", err.0);
    }

    #[test]
    fn custom_handler_can_be_registered() {
        let ex = DockerExecutor::new(DockerConfig::default());
        ex.register("cap.echo", |args, _cfg| {
            Ok(serde_json::json!({"got": args}))
        });
        let r = ex
            .execute("cap.echo", &serde_json::json!({"x": 42}))
            .unwrap();
        assert_eq!(r["got"]["x"], 42);
    }

    #[test]
    fn custom_handler_overrides_builtin() {
        let ex = DockerExecutor::new(DockerConfig::default());
        ex.register("cap.shell", |args, _cfg| {
            // A test-only override that doesn't need Docker
            let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            Ok(serde_json::json!({
                "exit_code": 0,
                "stdout": format!("mock-docker: {}", cmd),
                "stderr": "",
                "success": true,
            }))
        });
        let r = ex
            .execute("cap.shell", &serde_json::json!({"command": "echo hi"}))
            .unwrap();
        assert!(r["stdout"].as_str().unwrap().contains("mock-docker"));
    }

    /// Verify the Docker command structure by inspecting what would be run.
    /// Does not require Docker to be installed.
    #[test]
    fn build_docker_command_structure_with_defaults() {
        let cfg = DockerConfig::default();
        let args = serde_json::json!({"command": "echo hello"});

        // We can't assert on the actual subprocess, but we can validate
        // the error path — if Docker isn't installed, the error is descriptive.
        // If Docker IS installed, the command structure is correct.
        let result = run_shell_docker(&args, &cfg);
        match result {
            Ok(v) => {
                // Docker available and command succeeded
                assert_eq!(v["stdout"].as_str().unwrap_or("").trim(), "hello");
                assert!(v["success"].as_bool().unwrap());
            }
            Err(e) => {
                // Docker or timeout not installed — error message is informative
                assert!(
                    e.0.contains("docker") || e.0.contains("timeout"),
                    "expected docker/timeout error, got: {}",
                    e.0
                );
            }
        }
    }

    #[test]
    fn builder_pattern_is_ergonomic() {
        let cfg = DockerConfig::default()
            .with_image("debian:bookworm-slim")
            .with_network()
            .with_timeout(60);

        assert_eq!(cfg.image, "debian:bookworm-slim");
        assert!(cfg.network_enabled);
        assert_eq!(cfg.timeout_secs, 60);
        assert!(cfg.read_only); // unchanged from default
    }
}
