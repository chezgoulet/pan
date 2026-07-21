//! # Agent pool — load and serve [`AssembledAgent`]s by name.

use std::collections::HashMap;
use std::path::Path;

use pan_agent::builtin::builtin_registry;
use pan_agent::manifest::AgentManifest;
use pan_agent::AssembledAgent;

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
            let agent = pan_agent::assemble(&manifest, &registry)
                .map_err(|e| format!("{}: {e}", path.display()))?;
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
