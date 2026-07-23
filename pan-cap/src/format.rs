//! # `cap.format` — automatic code formatting.
//!
//! Formats files by extension after they are written. The agent calls
//! `cap.format.run { path: "/path/to/file.rs" }` and the capability
//! dispatches to the appropriate formatter (`rustfmt` for `.rs`,
//! `prettier` for `.js`/`.ts`/`.json`/`.md`/`.yaml`, etc.).
//!
//! The governor decides whether the agent may format files; the
//! capability itself just runs formatters.

use std::path::Path;

use pan_core::pipeline::ExecError;
use pan_core::schema::{Capability, Value};
use pan_core::toolbox::CapabilityProvider;

/// Formatter capability: runs language-specific formatters by file extension.
pub struct FormatCaps;

impl Default for FormatCaps {
    fn default() -> Self {
        Self
    }
}

impl FormatCaps {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl CapabilityProvider for FormatCaps {
    fn id(&self) -> &str {
        "cap.format"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability {
            id: "cap.format.run".into(),
            summary: "Format a source file using the appropriate language formatter.".into(),
            args_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to format"
                    }
                },
                "required": ["path"]
            }),
        }]
    }

    async fn execute(&self, _capability: &str, args: &Value) -> Result<Value, ExecError> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExecError("`path` must be a string".into()))?;

        let formatter = match Path::new(path).extension().and_then(|e| e.to_str()) {
            Some("rs") => "rustfmt",
            Some("js" | "ts" | "jsx" | "tsx") => "npx prettier --write",
            Some("json" | "jsonc") => "npx prettier --write",
            Some("md") => "npx prettier --write",
            Some("yaml" | "yml") => "npx prettier --write",
            Some("css" | "scss" | "less") => "npx prettier --write",
            Some("py") => "ruff format",
            Some("go") => "gofmt",
            Some("toml") => "taplo format",
            _ => return Err(ExecError(format!("no formatter registered for `{path}`"))),
        };

        let status = std::process::Command::new("sh")
            .args(["-c", &format!("{formatter} {path}")])
            .status()
            .map_err(|e| ExecError(format!("failed to run formatter: {e}")))?;

        if status.success() {
            Ok(serde_json::json!({ "formatted": path }))
        } else {
            Err(ExecError(format!(
                "{formatter} {path} exited with {status}"
            )))
        }
    }
}
