//! # `cap.skill` — skill lifecycle capabilities.
//!
//! Provides `skill.create`, `skill.edit`, `skill.list`, and `skill.delete`,
//! all confined to a root directory (the skills jail). The governor decides
//! *whether* a persona may reach `cap.skill`; this component defends the
//! path boundary — skills cannot be written outside the root.
//!
//! `skill.run` is deferred: executing a skill requires a [`ScopedInvoker`],
//! which the current [`execute`](CapabilityProvider::execute) contract does
//! not provide. When that plumbing lands, add `skill.run` here.

use std::path::{Component, Path, PathBuf};

use pan_core::invoker::ScopedInvoker;
use pan_core::pipeline::ExecError;
use pan_core::schema::{Capability, Value};
use pan_core::toolbox::CapabilityProvider;

use pan_skill::SkillRunner;

pub struct SkillCaps {
    root: PathBuf,
    /// Where the `pan.py` library is materialized for skill subprocesses.
    lib_dir: PathBuf,
}

impl SkillCaps {
    pub fn new(root: impl Into<PathBuf>, lib_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            lib_dir: lib_dir.into(),
        }
    }

    fn jail(&self, name: &str) -> Result<PathBuf, ExecError> {
        let p = Path::new(name);
        if p.is_absolute() {
            return Err(ExecError(format!("absolute path `{name}` is not allowed")));
        }
        for comp in p.components() {
            match comp {
                Component::ParentDir => {
                    return Err(ExecError(format!("`..` in `{name}` is not allowed")))
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(ExecError(format!("`{name}` must be relative")))
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

    /// Ensure the directory for a skill file exists.
    fn ensure_dir(&self, path: &Path) -> Result<(), ExecError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ExecError(format!("mkdir: {e}")))?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl CapabilityProvider for SkillCaps {
    fn id(&self) -> &str {
        "cap.skill"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability {
                id: "cap.skill.create".into(),
                summary: "create a new skill file under the agent's skill directory".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "required": ["name", "source"],
                    "properties": {
                        "name": { "type": "string", "description": "skill filename (e.g. summarizer.py)" },
                        "source": { "type": "string", "description": "full Python source code" }
                    }
                }),
            },
            Capability {
                id: "cap.skill.edit".into(),
                summary: "overwrite an existing skill file's source".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "required": ["name", "source"],
                }),
            },
            Capability {
                id: "cap.skill.list".into(),
                summary: "list all skill files in the agent's skill directory".into(),
                args_schema: serde_json::json!({ "type": "object" }),
            },
            Capability {
                id: "cap.skill.delete".into(),
                summary: "delete a skill file".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "required": ["name"],
                }),
            },
            Capability {
                id: "cap.skill.run".into(),
                summary: "run a Python skill through the governed pipeline".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": { "type": "string", "description": "skill filename (e.g. summarizer.py)" },
                        "input": { "type": "object", "description": "optional JSON input for the skill" }
                    }
                }),
            },
        ]
    }

    async fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError> {
        match capability {
            "cap.skill.run" => Err(ExecError("cap.skill.run requires a ScopedInvoker".into())),
            "cap.skill.create" => {
                let name = Self::arg_str(args, "name")?;
                let source = Self::arg_str(args, "source")?;
                let path = self.jail(name)?;
                if path.exists() {
                    return Err(ExecError(format!("skill `{name}` already exists")));
                }
                self.ensure_dir(&path)?;
                std::fs::write(&path, source).map_err(|e| ExecError(format!("write: {e}")))?;
                Ok(serde_json::json!({ "ok": true, "path": name }))
            }
            "cap.skill.edit" => {
                let name = Self::arg_str(args, "name")?;
                let source = Self::arg_str(args, "source")?;
                let path = self.jail(name)?;
                if !path.exists() {
                    return Err(ExecError(format!("skill `{name}` does not exist")));
                }
                self.ensure_dir(&path)?;
                std::fs::write(&path, source).map_err(|e| ExecError(format!("write: {e}")))?;
                Ok(serde_json::json!({ "ok": true, "path": name }))
            }
            "cap.skill.list" => {
                let mut skills = Vec::new();
                if self.root.exists() {
                    let dir = std::fs::read_dir(&self.root)
                        .map_err(|e| ExecError(format!("list: {e}")))?;
                    for entry in dir {
                        let entry = entry.map_err(|e| ExecError(format!("list: {e}")))?;
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.ends_with(".py") || name.ends_with(".toml") {
                            skills.push(name);
                        }
                    }
                }
                skills.sort();
                Ok(serde_json::json!({ "skills": skills }))
            }
            "cap.skill.delete" => {
                let name = Self::arg_str(args, "name")?;
                let path = self.jail(name)?;
                if !path.exists() {
                    return Err(ExecError(format!("skill `{name}` does not exist")));
                }
                std::fs::remove_file(&path).map_err(|e| ExecError(format!("delete: {e}")))?;
                Ok(serde_json::json!({ "ok": true }))
            }
            other => Err(ExecError(format!("cap.skill has no `{other}`"))),
        }
    }

    async fn execute_with_invoker(
        &self,
        capability: &str,
        args: &Value,
        invoker: &dyn ScopedInvoker,
    ) -> Result<Value, ExecError> {
        if capability != "cap.skill.run" {
            return self.execute(capability, args).await;
        }
        let name = Self::arg_str(args, "name")?;
        let path = self.jail(name)?;
        if !path.exists() {
            return Err(ExecError(format!("skill `{name}` does not exist")));
        }
        let input = args.get("input").cloned().unwrap_or(Value::Null);
        let runner =
            SkillRunner::new(&self.lib_dir).map_err(|e| ExecError(format!("skill runner: {e}")))?;
        runner
            .run(&path, &input, invoker)
            .await
            .map(|v| serde_json::json!({ "result": v }))
            .map_err(|e| ExecError(format!("skill run: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pan_cap_skill_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn temp_lib() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static M: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pan_cap_skill_lib_{}_{}",
            std::process::id(),
            M.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn create_then_list_round_trips() {
        let s = SkillCaps::new(temp_root(), temp_lib());
        s.execute(
            "cap.skill.create",
            &serde_json::json!({ "name": "summarize.py", "source": "import pan\nprint('ok')" }),
        )
        .await
        .unwrap();
        let got = s.execute("cap.skill.list", &Value::Null).await.unwrap();
        assert!(got["skills"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("summarize.py")),);
    }

    #[tokio::test]
    async fn create_then_edit_then_read_back() {
        let root = temp_root();
        let s = SkillCaps::new(&root, temp_lib());
        s.execute(
            "cap.skill.create",
            &serde_json::json!({ "name": "hello.py", "source": "v1" }),
        )
        .await
        .unwrap();
        s.execute(
            "cap.skill.edit",
            &serde_json::json!({ "name": "hello.py", "source": "v2" }),
        )
        .await
        .unwrap();
        let content = std::fs::read_to_string(root.join("hello.py")).unwrap();
        assert_eq!(content, "v2");
    }

    #[tokio::test]
    async fn delete_removes_the_file() {
        let root = temp_root();
        let s = SkillCaps::new(&root, temp_lib());
        s.execute(
            "cap.skill.create",
            &serde_json::json!({ "name": "tmp.py", "source": "x" }),
        )
        .await
        .unwrap();
        assert!(root.join("tmp.py").exists());
        s.execute("cap.skill.delete", &serde_json::json!({ "name": "tmp.py" }))
            .await
            .unwrap();
        assert!(!root.join("tmp.py").exists());
    }

    #[tokio::test]
    async fn path_traversal_is_refused() {
        let s = SkillCaps::new(temp_root(), temp_lib());
        let err = s
            .execute(
                "cap.skill.create",
                &serde_json::json!({ "name": "../escape.py", "source": "x" }),
            )
            .await
            .unwrap_err();
        assert!(err.0.contains(".."), "traversal must be refused: {}", err.0);
    }

    #[tokio::test]
    async fn duplicate_create_is_an_error() {
        let s = SkillCaps::new(temp_root(), temp_lib());
        s.execute(
            "cap.skill.create",
            &serde_json::json!({ "name": "dup.py", "source": "a" }),
        )
        .await
        .unwrap();
        let err = s
            .execute(
                "cap.skill.create",
                &serde_json::json!({ "name": "dup.py", "source": "b" }),
            )
            .await
            .unwrap_err();
        assert!(err.0.contains("already exists"));
    }

    #[tokio::test]
    async fn create_write_then_execute_without_invoker_reports_no_invoker() {
        let s = SkillCaps::new(temp_root(), temp_lib());
        // Creating a skill requires no invoker.
        s.execute(
            "cap.skill.create",
            &serde_json::json!({ "name": "a.py", "source": "x" }),
        )
        .await
        .unwrap();
        // Running without invoker gives a clear error.
        let err = s
            .execute("cap.skill.run", &serde_json::json!({ "name": "a.py" }))
            .await
            .unwrap_err();
        assert!(err.0.contains("ScopedInvoker"), "got: {}", err.0);
    }

    #[tokio::test]
    async fn run_without_invoker_gives_clear_message() {
        let s = SkillCaps::new(temp_root(), temp_lib());
        s.execute(
            "cap.skill.create",
            &serde_json::json!({ "name": "simple.py", "source": r#"import pan; pan.done({"echo": pan.input()})"# }),
        )
        .await
        .unwrap();
        let err = s
            .execute("cap.skill.run", &serde_json::json!({ "name": "simple.py" }))
            .await
            .unwrap_err();
        assert!(
            err.0.contains("ScopedInvoker"),
            "run without invoker should give clear error: {}",
            err.0
        );
    }
}
