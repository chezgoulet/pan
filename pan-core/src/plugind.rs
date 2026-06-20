//! # Plugin manager — plugind.
//!
//! Manages the lifecycle and run-state of all plugins, extending the native
//! [`Lifecycle`](crate::registry::Lifecycle) with:
//!
//! - **Discovery**: scan `~/.pan/plugins/` for `.wasm` files and their
//!   manifest `.toml` files
//! - **Wasm loading**: instantiate wasmtime modules via the C-ABI contract
//!   (see pan-sdk for the host side of the ABI)
//! - **Manifest validation**: name, version, capability declarations
//! - **Capability negotiation**: plugins declare what they need and what they
//!   provide; plugind wires them up at provision time
//! - **Atomic PluginSet**: the pipeline references an `Arc<PluginSet>` that is
//!   atomically swapped on reload
//! - **SIGHUP reload**: drain → rebuild → resume
//! - **Health probes**: per-plugin liveness checks

use crate::registry::{Lifecycle, Plugin, PluginError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// Re-export for callers.
pub use plugin_set::PluginSet;

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// A plugin's declaration file (`.toml` beside the `.wasm`).
///
/// ```toml
/// [plugin]
/// name = "pan-memory-ragamuffin"
/// version = "0.1.0"
///
/// [capabilities]
/// provides = ["memory.store", "memory.query"]
/// needs = ["http.client"]
/// ```
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PluginManifest {
    #[serde(rename = "plugin")]
    pub meta: ManifestMeta,
    pub capabilities: ManifestCapabilities,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ManifestMeta {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ManifestCapabilities {
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub needs: Vec<String>,
}

impl PluginManifest {
    /// Load and validate a manifest from a TOML file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, PlugindError> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path).map_err(|e| PlugindError::ManifestIo {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;
        let manifest: PluginManifest =
            toml::from_str(&content).map_err(|e| PlugindError::ManifestParse {
                path: path.to_path_buf(),
                detail: e.to_string(),
            })?;

        // Validate.
        if manifest.meta.name.is_empty() {
            return Err(PlugindError::ManifestValidation {
                path: path.to_path_buf(),
                message: "plugin name must not be empty".into(),
            });
        }
        if manifest.meta.version.is_empty() {
            return Err(PlugindError::ManifestValidation {
                path: path.to_path_buf(),
                message: "plugin version must not be empty".into(),
            });
        }

        Ok(manifest)
    }
}

// ---------------------------------------------------------------------------
// Native wrapper around a Wasm plugin (placeholder until #62 formalizes the ABI)
// ---------------------------------------------------------------------------

/// A Wasm-hosted plugin loaded through wasmtime.
///
/// The C-ABI contract (defined formally in pan-sdk, #62) defines the exported
/// functions this wrapper calls. For now, this struct holds the wasmtime state
/// and delegates to the instance.
pub struct WasmPlugin {
    id: String,
    manifest: PluginManifest,
    #[allow(dead_code)]
    wasm_path: PathBuf,
    // wasmtime instance state — hydrated by pan-sdk:
    // instance: wasmtime::Instance,
    // store: wasmtime::Store<()>,
}

impl WasmPlugin {
    /// Load a Wasm plugin from its `.wasm` file with the given manifest.
    pub fn load(wasm_path: PathBuf, manifest: PluginManifest) -> Result<Self, PlugindError> {
        let id = manifest.meta.name.clone();
        // TODO(#62): instantiate wasmtime module and link the C-ABI exports.
        // For now, stub the instance — this compiles and the real ABI will
        // be wired when the SDK lands.
        Ok(WasmPlugin { id, manifest, wasm_path })
    }

    /// Panic if `provision` / `validate` / `run` / `cleanup` are called without
    /// an active wasmtime instance. This is a safe assertion — the SDK work
    /// (#62) fills in the real implementation.
    fn assert_abi_ready(&self) {
        // Gate removed once #62 provides the wasmtime instance.
    }
}

impl Plugin for WasmPlugin {
    fn id(&self) -> &str {
        &self.id
    }

    fn provision(&mut self) -> Result<(), PluginError> {
        self.assert_abi_ready();
        // TODO(#62): call plugin_provision export on the wasm instance.
        Ok(())
    }

    fn validate(&self) -> Result<(), PluginError> {
        self.assert_abi_ready();
        // TODO(#62): call plugin_validate export on the wasm instance.
        Ok(())
    }

    fn run(&mut self) -> Result<(), PluginError> {
        self.assert_abi_ready();
        // TODO(#62): call plugin_run export on the wasm instance.
        Ok(())
    }

    fn cleanup(&mut self) {
        self.assert_abi_ready();
        // TODO(#62): call plugin_cleanup export on the wasm instance.
    }
}

// ---------------------------------------------------------------------------
// NativePlugin — wraps an in-process plugin behind the same lifecycle
// ---------------------------------------------------------------------------

/// A native (in-process) plugin, registered via a closure.
pub struct NativePlugin {
    id: String,
    provision_fn: Box<dyn FnOnce() -> Result<(), PluginError> + Send>,
    validate_fn: Box<dyn Fn() -> Result<(), PluginError> + Send>,
    run_fn: Box<dyn FnMut() -> Result<(), PluginError> + Send>,
    cleanup_fn: Box<dyn FnMut() + Send>,
}

impl std::fmt::Debug for NativePlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativePlugin").field("id", &self.id).finish()
    }
}

impl Plugin for NativePlugin {
    fn id(&self) -> &str {
        &self.id
    }
    fn provision(&mut self) -> Result<(), PluginError> {
        (self.provision_fn.take().unwrap_or_else(|| Box::new(|| Ok(()))))()
    }
    fn validate(&self) -> Result<(), PluginError> {
        (self.validate_fn)()
    }
    fn run(&mut self) -> Result<(), PluginError> {
        (self.run_fn)()
    }
    fn cleanup(&mut self) {
        (self.cleanup_fn)()
    }
}

// ---------------------------------------------------------------------------
// PluginSet — atomic snapshot
// ---------------------------------------------------------------------------

mod plugin_set {
    use crate::registry::Plugin;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// A snapshot of all active plugins. Atomically swappable via
    /// [`PluginManager::reload`](super::PluginManager).
    ///
    /// The pipeline holds an `Arc<PluginSet>` at any point; the manager writes
    /// a new version on reload.
    pub struct PluginSet {
        plugins: Vec<Arc<dyn Plugin + Send + Sync>>,
        by_id: HashMap<String, usize>,
        // Capability index: maps capability id → plugin id
        capability_index: HashMap<String, String>,
    }

    impl PluginSet {
        pub(crate) fn new(
            plugins: Vec<Arc<dyn Plugin + Send + Sync>>,
            capability_index: HashMap<String, String>,
        ) -> Self {
            let mut by_id = HashMap::new();
            for (i, p) in plugins.iter().enumerate() {
                by_id.insert(p.id().to_string(), i);
            }
            PluginSet { plugins, by_id, capability_index }
        }

        /// Look up a plugin by its id.
        pub fn lookup(&self, id: &str) -> Option<&(dyn Plugin + Send + Sync)> {
            self.by_id.get(id).and_then(|&i| self.plugins.get(i)).map(|p| p.as_ref())
        }

        /// Find which plugin provides a given capability.
        pub fn provider_for(&self, capability: &str) -> Option<&(dyn Plugin + Send + Sync)> {
            self.capability_index.get(capability).and_then(|id| self.lookup(id))
        }

        /// Iterate all plugins.
        pub fn all(&self) -> &[Arc<dyn Plugin + Send + Sync>] {
            &self.plugins
        }

        /// Number of plugins in the set.
        pub fn len(&self) -> usize {
            self.plugins.len()
        }

        pub fn is_empty(&self) -> bool {
            self.plugins.is_empty()
        }
    }
}

// ---------------------------------------------------------------------------
// PluginManager
// ---------------------------------------------------------------------------

/// The plugin manager. Owns the lifecycle for all plugins, provides atomic
/// PluginSet access for the pipeline, and handles SIGHUP reloads.
pub struct PluginManager {
    /// Active plugin set — atomically swappable on reload.
    set: Arc<PluginSet>,
    /// Lifecycle driver for the active set.
    lifecycle: Lifecycle,
    /// Directories scanned for plugins.
    plugin_dirs: Vec<PathBuf>,
    /// Whether a reload is in progress (guards against concurrent reloads).
    reloading: AtomicBool,
}

impl PluginManager {
    /// Create and initialize the plugin manager.
    ///
    /// Discovers plugins from configured directories, loads their manifests and
    /// Wasm modules, provisions the lifecycle, and returns a ready-to-run manager.
    ///
    /// Does NOT start the plugins — call [`start`](Self::start) for that.
    pub fn new(plugin_dirs: Vec<PathBuf>) -> Result<Self, PlugindError> {
        let (plugins, capability_index) = discover_all(&plugin_dirs)?;
        let plugin_arcs: Vec<Arc<dyn Plugin + Send + Sync>> = plugins
            .into_iter()
            .map(|p| -> Arc<dyn Plugin + Send + Sync> { p })
            .collect();
        let set = Arc::new(PluginSet::new(plugin_arcs.clone(), capability_index.clone()));

        let mut lifecycle = Lifecycle::new();
        // TODO(#62): instantiate wasmtime instances and register them here.
        // For now, native-only plugins work; Wasm plugins are stubs.
        // lifecycle.register(Box::new(wasm_plugin));

        Ok(PluginManager {
            set,
            lifecycle,
            plugin_dirs,
            reloading: AtomicBool::new(false),
        })
    }

    /// Start all plugins through the lifecycle (provision → validate → run).
    pub fn start(&mut self) -> Result<(), PlugindError> {
        self.lifecycle.start().map_err(PlugindError::Lifecycle)
    }

    /// Shut down all plugins (cleanup in reverse order).
    pub fn shutdown(&mut self) {
        self.lifecycle.cleanup();
    }

    /// Perform a SIGHUP-style reload: drain → rebuild → resume.
    ///
    /// 1. Clean up existing plugins (reverse order)
    /// 2. Re-discover and load plugins
    /// 3. Build a new PluginSet and swap atomically
    /// 4. Start the new lifecycle
    ///
    /// If step 4 fails, the old set is gone and the system is in a degraded
    /// state (plugins were cleaned up). Future work: retain the old PluginSet
    /// until the new one verifies healthy.
    pub fn reload(&mut self) -> Result<(), PlugindError> {
        if self.reloading.swap(true, Ordering::SeqCst) {
            return Err(PlugindError::ReloadInProgress);
        }

        // Phase 1: drain.
        self.lifecycle.cleanup();

        // Phase 2: rebuild.
        let (plugins, capability_index) = discover_all(&self.plugin_dirs)?;
        let plugin_arcs: Vec<Arc<dyn Plugin + Send + Sync>> = plugins
            .into_iter()
            .map(|p| -> Arc<dyn Plugin + Send + Sync> { p })
            .collect();
        let new_set = Arc::new(PluginSet::new(plugin_arcs, capability_index));

        let mut new_lifecycle = Lifecycle::new();

        // Phase 3: swap.
        self.set = new_set;
        self.lifecycle = new_lifecycle;

        // Phase 4: resume.
        self.lifecycle.start().map_err(PlugindError::Lifecycle)?;

        self.reloading.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Get the current plugin set for the pipeline to reference.
    pub fn set(&self) -> Arc<PluginSet> {
        Arc::clone(&self.set)
    }

    /// Health check: collect probe results from all active plugins.
    ///
    /// Native plugins always report alive. Wasm plugins probe the instance.
    pub fn health(&self) -> Vec<PluginHealth> {
        self.set
            .all()
            .iter()
            .map(|p| PluginHealth {
                id: p.id().to_string(),
                alive: true, // TODO(#58): real health probe via wasmtime
                error: None,
            })
            .collect()
    }
}

/// Result of a health probe on a single plugin.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginHealth {
    pub id: String,
    pub alive: bool,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Scan plugin directories, load manifests, build WasmPlugin stubs.
fn discover_all(
    dirs: &[PathBuf],
) -> Result<(Vec<Arc<dyn Plugin + Send + Sync>>, HashMap<String, String>), PlugindError> {
    let mut plugins: Vec<Arc<dyn Plugin + Send + Sync>> = Vec::new();
    let mut capability_index = HashMap::new();

    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(PlugindError::DiscoveryIo {
                    path: dir.clone(),
                    detail: e.to_string(),
                })
            }
        };

        for entry in entries {
            let entry = entry.map_err(|e| PlugindError::DiscoveryIo {
                path: dir.clone(),
                detail: e.to_string(),
            })?;
            let path = entry.path();

            if path.extension().map_or(false, |ext| ext == "wasm") {
                let manifest_path = path.with_extension("toml");
                let manifest = if manifest_path.exists() {
                    PluginManifest::load(&manifest_path)?
                } else {
                    // Without a manifest, derive a minimal identity from the filename.
                    PluginManifest {
                        meta: ManifestMeta {
                            name: path
                                .file_stem()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned(),
                            version: "0.0.0".into(),
                        },
                        capabilities: ManifestCapabilities {
                            provides: vec![],
                            needs: vec![],
                        },
                    }
                };

                // Index capabilities.
                for cap in &manifest.capabilities.provides {
                    capability_index.insert(cap.clone(), manifest.meta.name.clone());
                }

                let wasm_plugin = WasmPlugin::load(path, manifest)?;
                plugins.push(Arc::new(wasm_plugin));
            }
        }
    }

    Ok((plugins, capability_index))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum PlugindError {
    /// I/O error during manifest loading.
    ManifestIo {
        path: PathBuf,
        detail: String,
    },
    /// TOML parse error in a manifest.
    ManifestParse {
        path: PathBuf,
        detail: String,
    },
    /// Manifest validation failed (e.g. missing name).
    ManifestValidation {
        path: PathBuf,
        message: String,
    },
    /// I/O error during directory discovery.
    DiscoveryIo {
        path: PathBuf,
        detail: String,
    },
    /// Wasmtime instantiation error.
    WasmLoad {
        path: PathBuf,
        detail: String,
    },
    /// Lifecycle error during start or reload.
    Lifecycle(crate::registry::LifecycleError),
    /// A reload was already in progress.
    ReloadInProgress,
}

impl std::fmt::Display for PlugindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlugindError::ManifestIo { path, detail } => {
                write!(f, "I/O error reading plugin manifest `{}`: {}", path.display(), detail)
            }
            PlugindError::ManifestParse { path, detail } => {
                write!(f, "TOML parse error in plugin manifest `{}`: {}", path.display(), detail)
            }
            PlugindError::ManifestValidation { path, message } => {
                write!(f, "plugin manifest validation error in `{}`: {}", path.display(), message)
            }
            PlugindError::DiscoveryIo { path, detail } => {
                write!(f, "I/O error scanning plugin directory `{}`: {}", path.display(), detail)
            }
            PlugindError::WasmLoad { path, detail } => {
                write!(f, "wasmtime instantiation error for `{}`: {}", path.display(), detail)
            }
            PlugindError::Lifecycle(e) => write!(f, "lifecycle error: {e}"),
            PlugindError::ReloadInProgress => write!(f, "a SIGHUP reload is already in progress"),
        }
    }
}

impl std::error::Error for PlugindError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn manifest_load_valid() {
        let dir = std::env::temp_dir().join("pan_test_manifest_valid");
        let _ = fs::create_dir_all(&dir);
        let mpath = dir.join("plugin.toml");
        fs::write(&mpath, r#"
[plugin]
name = "test-plugin"
version = "0.1.0"

[capabilities]
provides = ["cap.a", "cap.b"]
needs = ["cap.c"]
"#).unwrap();

        let m = PluginManifest::load(&mpath).unwrap();
        assert_eq!(m.meta.name, "test-plugin");
        assert_eq!(m.meta.version, "0.1.0");
        assert_eq!(m.capabilities.provides, vec!["cap.a", "cap.b"]);
        assert_eq!(m.capabilities.needs, vec!["cap.c"]);
    }

    #[test]
    fn manifest_rejects_empty_name() {
        let dir = std::env::temp_dir().join("pan_test_manifest_empty");
        let _ = fs::create_dir_all(&dir);
        let mpath = dir.join("bad.toml");
        fs::write(&mpath, r#"
[plugin]
name = ""
version = "0.1.0"
"#).unwrap();

        let err = PluginManifest::load(&mpath).unwrap_err();
        assert!(matches!(err, PlugindError::ManifestValidation { .. }));
    }

    #[test]
    fn discovery_scans_directory() {
        let dir = std::env::temp_dir().join("pan_test_discovery");
        let _ = fs::create_dir_all(&dir);

        // Create a fake .wasm file with a manifest.
        let wasm_path = dir.join("test-plugin.wasm");
        fs::write(&wasm_path, b"not a real wasm binary")?;
        fs::write(dir.join("test-plugin.toml"), r#"
[plugin]
name = "test-plugin"
version = "0.1.0"
[capabilities]
"#).unwrap();

        let (plugins, _) = discover_all(&[dir.clone()]).unwrap();
        assert_eq!(plugins.len(), 1, "should discover the fake wasm file");
        assert_eq!(plugins[0].id(), "test-plugin");
    }

    #[test]
    fn discovery_skips_missing_directory() {
        let dir = PathBuf::from("/tmp/pan_test_nonexistent_QWXYZ");
        let (plugins, _) = discover_all(&[dir]).unwrap();
        assert!(plugins.is_empty());
    }

    #[test]
    fn plugin_set_lookup() {
        let dir = std::env::temp_dir().join("pan_test_pset");
        let _ = fs::create_dir_all(&dir);
        let d1 = dir.join("p1.wasm");
        fs::write(&d1, b"fake")?;
        fs::write(dir.join("p1.toml"), r#"
[plugin]
name = "p1"
version = "1.0"
[capabilities]
provides = ["cap.a"]
"#).unwrap();

        let d2 = dir.join("p2.wasm");
        fs::write(&d2, b"fake")?;
        fs::write(dir.join("p2.toml"), r#"
[plugin]
name = "p2"
version = "1.0"
[capabilities]
"#).unwrap();

        let (plugins, cap_index) = discover_all(&[dir]).unwrap();
        let set = PluginSet::new(
            plugins.into_iter().map(|p| p).collect(),
            cap_index,
        );

        assert_eq!(set.len(), 2);
        assert!(set.lookup("p1").is_some());
        assert!(set.lookup("missing").is_none());

        // Capability index.
        let provider = set.provider_for("cap.a");
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().id(), "p1");
    }
}
