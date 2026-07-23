//! # `cap.lsp` — language diagnostics for agent feedback.
//!
//! Runs per-extension checkers (rustfmt --check, ruff, tsc, go vet, etc.)
//! to surface diagnostics the agent can act on.

use std::path::Path;

use pan_core::pipeline::ExecError;
use pan_core::schema::{Capability, Value};
use pan_core::toolbox::CapabilityProvider;

pub struct LspCaps;

impl Default for LspCaps {
    fn default() -> Self {
        Self
    }
}

impl LspCaps {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl CapabilityProvider for LspCaps {
    fn id(&self) -> &str {
        "cap.lsp"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability {
                id: "cap.lsp.check".into(),
                summary: "Run diagnostics on a file (language-appropriate linter/checker).".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "path to the file to check" }
                    },
                    "required": ["path"]
                }),
            },
            Capability {
                id: "cap.lsp.format".into(),
                summary: "Check whether a file is correctly formatted.".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                }),
            },
        ]
    }

    async fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExecError("`path` must be a string".into()))?;
        let file_path = Path::new(path);
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

        match capability {
            "cap.lsp.check" => run_diagnostics(ext, path),
            "cap.lsp.format" => check_format(ext, path),
            _ => Err(ExecError(format!("cap.lsp has no `{capability}`"))),
        }
    }
}

fn run_diagnostics(ext: &str, path: &str) -> Result<Value, ExecError> {
    let (cmd, args) = match ext {
        "rs" => (
            "rustc",
            vec!["--edition", "2021", "--crate-type", "lib", path],
        ),
        "py" => ("ruff", vec!["check", "--output-format=json", path]),
        "ts" | "tsx" => ("npx", vec!["tsc", "--noEmit", "--pretty", "false", path]),
        "js" | "jsx" | "mjs" => ("node", vec!["--check", path]),
        "go" => ("go", vec!["vet", path]),
        _ => {
            return Ok(
                serde_json::json!({ "diagnostics": [], "note": format!("no checker for .{ext}") }),
            )
        }
    };

    let output = std::process::Command::new(cmd)
        .args(&args)
        .output()
        .map_err(|e| ExecError(format!("failed to run `{cmd}`: {e}")))?;

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    // Parse structured output where possible; fall back to line-based stderr.
    if ext == "py" && !stdout.is_empty() {
        if let Ok(diags) = serde_json::from_str::<Value>(&stdout) {
            return Ok(serde_json::json!({ "diagnostics": diags }));
        }
    }

    // Generic: split stderr into diagnostic lines.
    let diags: Vec<Value> = stderr
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::json!({ "message": l.trim(), "severity": "error" }))
        .collect();

    Ok(serde_json::json!({
        "diagnostics": diags,
        "exit_code": output.status.code().unwrap_or(-1),
    }))
}

fn check_format(ext: &str, path: &str) -> Result<Value, ExecError> {
    let (cmd, args) = match ext {
        "rs" => ("rustfmt", vec!["--check", path]),
        "py" => (
            "ruff",
            vec!["format", "--check", "--output-format=json", path],
        ),
        "ts" | "tsx" | "js" | "jsx" | "json" | "md" | "yaml" | "yml" => {
            ("npx", vec!["prettier", "--check", path])
        }
        "go" => ("gofmt", vec!["-l", path]),
        _ => {
            return Ok(
                serde_json::json!({ "formatted": true, "note": format!("no formatter check for .{ext}") }),
            )
        }
    };

    let output = std::process::Command::new(cmd)
        .args(&args)
        .output()
        .map_err(|e| ExecError(format!("failed to run `{cmd}`: {e}")))?;

    let is_formatted = output.status.success();
    let details = String::from_utf8_lossy(if is_formatted {
        &output.stdout
    } else {
        &output.stderr
    })
    .into_owned();

    Ok(serde_json::json!({
        "formatted": is_formatted,
        "details": if details.is_empty() { Value::Null } else { Value::String(details) },
    }))
}
