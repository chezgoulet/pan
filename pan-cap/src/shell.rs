//! # `cap.shell` — run a program.
//!
//! Provides `cap.shell.run`: execute a program with arguments and return its exit
//! code, stdout, and stderr. It runs the program **directly** — no shell, so no
//! metacharacter interpretation, word-splitting, or globbing (`args` is an
//! explicit list). That removes shell-injection as a class.
//!
//! This is a powerful capability, and it is opt-in twice: a persona must `enable`
//! it *and* be `grant`ed `cap.shell`. Argument-level policy (an allowlist of
//! programs / a regex on args) belongs in the governor and is a future
//! refinement; today the boundary is "may this persona reach `cap.shell` at all?"
//!
//! Blocking `std::process` inside the async method, matching `cap.fs` — fine for
//! short commands; a non-blocking variant is a later refinement.

use pan_core::pipeline::ExecError;
use pan_core::schema::{Capability, Value};
use pan_core::toolbox::CapabilityProvider;

/// The `cap.shell` component.
#[derive(Default)]
pub struct ShellCaps;

impl ShellCaps {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl CapabilityProvider for ShellCaps {
    fn id(&self) -> &str {
        "cap.shell"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability {
            id: "cap.shell.run".into(),
            summary: "run a program with arguments (no shell interpretation)".into(),
            args_schema: serde_json::json!({
                "type": "object",
                "required": ["program"]
            }),
        }]
    }

    async fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError> {
        if capability != "cap.shell.run" {
            return Err(ExecError(format!("cap.shell has no `{capability}`")));
        }
        let program = args
            .get("program")
            .and_then(|p| p.as_str())
            .ok_or_else(|| ExecError("`program` must be a string".into()))?;

        // `args` is an optional array of strings — explicit, never shell-split.
        let cmd_args: Vec<String> = match args.get("args") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| ExecError("each `args` entry must be a string".into()))
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(ExecError("`args` must be an array of strings".into())),
        };

        let output = std::process::Command::new(program)
            .args(&cmd_args)
            .output()
            .map_err(|e| ExecError(format!("spawning `{program}`: {e}")))?;

        Ok(serde_json::json!({
            "code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_a_program_and_captures_stdout() {
        let sh = ShellCaps::new();
        let out = sh
            .execute(
                "cap.shell.run",
                &serde_json::json!({ "program": "echo", "args": ["hello", "world"] }),
            )
            .await
            .unwrap();
        assert_eq!(out["code"], 0);
        assert_eq!(out["stdout"].as_str().unwrap().trim(), "hello world");
    }

    #[tokio::test]
    async fn nonzero_exit_is_reported_not_an_error() {
        let sh = ShellCaps::new();
        // `false` exits 1; that is a result, not an ExecError (the program ran).
        let out = sh
            .execute("cap.shell.run", &serde_json::json!({ "program": "false" }))
            .await
            .unwrap();
        assert_eq!(out["code"], 1);
    }

    #[tokio::test]
    async fn a_missing_program_is_an_exec_error() {
        let sh = ShellCaps::new();
        let err = sh
            .execute(
                "cap.shell.run",
                &serde_json::json!({ "program": "no_such_program_xyz_123" }),
            )
            .await
            .unwrap_err();
        assert!(err.0.contains("no_such_program_xyz_123"), "got: {}", err.0);
    }
}
