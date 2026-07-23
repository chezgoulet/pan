//! # Agent pool — load and serve [`AssembledAgent`]s by name.

use std::collections::HashMap;
use std::path::Path;

use pan_agent::builtin::builtin_registry;
use pan_agent::manifest::AgentManifest;
use pan_agent::AssembledAgent;
use pan_core::config::Config;

/// A collection of assembled agents, loaded from `Agent.toml` files and keyed
/// by `meta.name`.
pub struct AgentPool {
    agents: HashMap<String, AssembledAgent>,
    /// The directory the pool was loaded from, so delegate handlers can
    /// re-assemble child agents from their manifests.
    source_dir: Option<std::path::PathBuf>,
}

impl AgentPool {
    /// Load all `Agent.toml` files from `dir`. Non-recursive: only the top-level
    /// `*.toml` files whose `meta.name` is non-empty become pool entries.
    pub fn load(dir: &Path) -> Result<Self, String> {
        Self::load_with_config(dir, None)
    }

    /// Load with global config merging. Global settings from `~/.pan/config.toml`
    /// serve as defaults that per-agent `Agent.toml` settings override.
    pub fn load_with_config(dir: &Path, global: Option<&Config>) -> Result<Self, String> {
        let registry = builtin_registry();
        let mut agents = HashMap::new();
        let source_dir = Some(dir.to_path_buf());
        if !dir.exists() {
            return Err(format!("agents directory not found: {}", dir.display()));
        }
        for entry in std::fs::read_dir(dir).map_err(|e| format!("read_dir: {e}"))? {
            let entry = entry.map_err(|e| format!("entry: {e}"))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let manifest =
                AgentManifest::load(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if manifest.meta.name.trim().is_empty() {
                continue;
            }
            let name = manifest.meta.name.clone();
            let agent = match pan_agent::assemble_with_config(&manifest, &registry, global) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("pan-gateway: skipping `{}`: {e}", path.display());
                    continue;
                }
            };
            if agents.contains_key(&name) {
                return Err(format!("duplicate agent name `{name}` in pool"));
            }
            agents.insert(name, agent);
        }
        if agents.is_empty() {
            return Err("no agents loaded (no valid Agent.toml files found)".into());
        }
        Ok(Self { agents, source_dir })
    }

    /// Number of agents in the pool.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Look up an agent by name.
    pub fn get(&self, name: &str) -> Option<&AssembledAgent> {
        self.agents.get(name)
    }

    /// All agent names in the pool.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.agents.keys().map(String::as_str)
    }

    /// The source directory the pool was loaded from, for re-assembling agents.
    pub fn agent_dir(&self) -> Result<&std::path::Path, String> {
        self.source_dir
            .as_deref()
            .ok_or_else(|| "agent pool has no source directory".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use std::path::{Path, PathBuf};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("pan_gw_test_{label}_{}_{}", std::process::id(), n))
    }

    fn write_agent_toml(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(format!("{name}.toml")), content).unwrap();
    }

    fn echo_toml(name: &str) -> String {
        format!(
            r#"[meta]
name = "{name}"
persona = "assistant"

[persona]
instruction = "You are an echo."
provider = "provider.echo"

[caps.grant]
shell = true
state = true

[caps.settings."cap.state"]
path = "memory.json"
"#,
        )
    }

    #[test]
    fn load_pool_from_directory() {
        let dir = std::env::temp_dir().join(format!("pan_gw_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        write_agent_toml(&dir, "echo", &echo_toml("echo"));

        let pool = AgentPool::load(&dir).unwrap();
        assert_eq!(pool.len(), 1);
        assert!(pool.get("echo").is_some());
        assert_eq!(pool.names().collect::<Vec<_>>(), vec!["echo"]);
        assert!(pool.get("missing").is_none());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_pool_rejects_duplicate_names() {
        let dir = temp_dir("dup");
        fs::create_dir_all(&dir).unwrap();
        write_agent_toml(&dir, "echo", &echo_toml("echo"));
        write_agent_toml(&dir, "echo2", &echo_toml("echo"));

        match AgentPool::load(&dir) {
            Err(msg) => assert!(msg.contains("duplicate"), "expected duplicate error: {msg}"),
            Ok(_) => panic!("expected duplicate name error"),
        }

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_pool_empty_directory_is_error() {
        let dir = temp_dir("empty");
        fs::create_dir_all(&dir).unwrap();

        match AgentPool::load(&dir) {
            Err(msg) => assert!(msg.contains("no agents"), "expected no-agents error: {msg}"),
            Ok(_) => panic!("expected error for empty directory"),
        }

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_pool_missing_directory_is_error() {
        let dir = PathBuf::from("/tmp/pan_gw_test_nonexistent_UNLIKELY");
        match AgentPool::load(&dir) {
            Err(msg) => assert!(msg.contains("not found"), "expected not-found error: {msg}"),
            Ok(_) => panic!("expected error for missing directory"),
        }
    }

    #[test]
    fn load_pool_skips_non_toml_files() {
        let dir = temp_dir("skip");
        fs::create_dir_all(&dir).unwrap();
        write_agent_toml(&dir, "echo", &echo_toml("echo"));
        fs::write(dir.join("readme.txt"), "not an agent").unwrap();

        // Non-toml files are ignored; only 'echo.toml' is loaded.
        let pool = AgentPool::load(&dir).unwrap();
        assert_eq!(pool.len(), 1);

        fs::remove_dir_all(dir).unwrap();
    }
}
