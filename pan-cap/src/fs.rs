//! # `cap.fs` — rooted filesystem access.
//!
//! Provides `cap.fs.read`, `cap.fs.write`, and `cap.fs.list`, all confined to a
//! root directory. The governor decides *whether* a persona may reach `cap.fs`
//! at all; this component adds **defense in depth** at the executor: every path
//! is jailed under the root, and absolute paths or `..` traversal are refused
//! outright. So even a persona granted `cap.fs` cannot read outside its root.
//!
//! File I/O is blocking `std::fs` inside the async method — fine for the small
//! reads/writes a skill makes; a fully non-blocking variant is a later refinement
//! (the same trade-off the LLM client makes).

use std::path::{Component, Path, PathBuf};

use pan_core::pipeline::ExecError;
use pan_core::schema::{Capability, Value};
use pan_core::toolbox::CapabilityProvider;

/// Filesystem capabilities confined to `root`.
pub struct FsCaps {
    root: PathBuf,
}

impl FsCaps {
    /// Create an `cap.fs` component rooted at `root`. All paths are resolved
    /// relative to it and may not escape it.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve a caller-supplied relative path against the root, refusing
    /// anything that could escape it. This is the jail: absolute paths and any
    /// `..` component are rejected before touching the filesystem.
    fn jail(&self, rel: &str) -> Result<PathBuf, ExecError> {
        let p = Path::new(rel);
        if p.is_absolute() {
            return Err(ExecError(format!("absolute path `{rel}` is not allowed")));
        }
        for comp in p.components() {
            match comp {
                Component::ParentDir => {
                    return Err(ExecError(format!("`..` in `{rel}` is not allowed")))
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(ExecError(format!("`{rel}` must be relative")))
                }
                _ => {}
            }
        }
        Ok(self.root.join(p))
    }

    fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ExecError> {
        args.get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExecError(format!("`{key}` must be a string")))
    }
}

#[async_trait::async_trait]
impl CapabilityProvider for FsCaps {
    fn id(&self) -> &str {
        "cap.fs"
    }

    fn capabilities(&self) -> Vec<Capability> {
        let path_only = serde_json::json!({ "type": "object", "required": ["path"] });
        vec![
            Capability {
                id: "cap.fs.read".into(),
                summary: "read a UTF-8 file under the agent's root".into(),
                args_schema: path_only.clone(),
            },
            Capability {
                id: "cap.fs.write".into(),
                summary: "write a UTF-8 file under the agent's root".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "required": ["path", "content"]
                }),
            },
            Capability {
                id: "cap.fs.list".into(),
                summary: "list a directory under the agent's root".into(),
                args_schema: path_only,
            },
        ]
    }

    async fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError> {
        let path = self.jail(Self::arg_str(args, "path")?)?;
        match capability {
            "cap.fs.read" => {
                let content =
                    std::fs::read_to_string(&path).map_err(|e| ExecError(format!("read: {e}")))?;
                Ok(serde_json::json!({ "content": content }))
            }
            "cap.fs.write" => {
                let content = Self::arg_str(args, "content")?;
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| ExecError(format!("mkdir: {e}")))?;
                }
                std::fs::write(&path, content).map_err(|e| ExecError(format!("write: {e}")))?;
                Ok(serde_json::json!({ "bytes": content.len() }))
            }
            "cap.fs.list" => {
                let mut entries = Vec::new();
                let dir = std::fs::read_dir(&path).map_err(|e| ExecError(format!("list: {e}")))?;
                for entry in dir {
                    let entry = entry.map_err(|e| ExecError(format!("list: {e}")))?;
                    entries.push(entry.file_name().to_string_lossy().into_owned());
                }
                entries.sort();
                Ok(serde_json::json!({ "entries": entries }))
            }
            other => Err(ExecError(format!("cap.fs has no `{other}`"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pan_cap_fs_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let fs = FsCaps::new(temp_root());
        fs.execute(
            "cap.fs.write",
            &serde_json::json!({ "path": "a/b.txt", "content": "hi" }),
        )
        .await
        .unwrap();
        let got = fs
            .execute("cap.fs.read", &serde_json::json!({ "path": "a/b.txt" }))
            .await
            .unwrap();
        assert_eq!(got["content"], "hi");
    }

    #[tokio::test]
    async fn path_traversal_is_refused_even_within_cap_fs() {
        let fs = FsCaps::new(temp_root());
        let err = fs
            .execute("cap.fs.read", &serde_json::json!({ "path": "../secrets" }))
            .await
            .unwrap_err();
        assert!(err.0.contains(".."), "traversal must be refused: {}", err.0);
    }

    #[tokio::test]
    async fn absolute_paths_are_refused() {
        let fs = FsCaps::new(temp_root());
        let err = fs
            .execute("cap.fs.read", &serde_json::json!({ "path": "/etc/passwd" }))
            .await
            .unwrap_err();
        assert!(err.0.contains("absolute"), "got: {}", err.0);
    }
}
