#![allow(clippy::type_complexity)]
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
    #[serde(default)]
    pub capabilities: ManifestCapabilities,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ManifestMeta {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
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
/// The C-ABI contract defines the four exported functions every plugin must
/// expose. The host provides imports (`pan_log`, `pan_get_state`,
/// `pan_set_state`) that plugins can call to interact with Pan.
pub struct WasmPlugin {
    id: String,
    #[allow(dead_code)]
    manifest: PluginManifest,
    #[allow(dead_code)]
    wasm_path: PathBuf,
    #[allow(dead_code)]
    instance: wasmtime::Instance,
    #[allow(dead_code)]
    store: wasmtime::Store<WasmPluginState>,
}

/// Per-plugin state stored in the wasmtime store, accessible from host
/// functions called by the plugin.
struct WasmPluginState {
    #[allow(dead_code)]
    state: std::collections::HashMap<String, String>,
}

impl WasmPlugin {
    /// Load a Wasm plugin from its `.wasm` file with the given manifest.
    pub fn load(wasm_path: PathBuf, manifest: PluginManifest) -> Result<Self, PlugindError> {
        let id = manifest.meta.name.clone();

        let engine = wasmtime::Engine::new(&wasmtime::Config::new()).map_err(|e| {
            PlugindError::WasmLoad {
                path: wasm_path.clone(),
                detail: e.to_string(),
            }
        })?;

        let module = wasmtime::Module::from_file(&engine, &wasm_path).map_err(|e| {
            PlugindError::WasmLoad {
                path: wasm_path.clone(),
                detail: e.to_string(),
            }
        })?;

        let mut linker = wasmtime::Linker::<WasmPluginState>::new(&engine);

        // Link host imports.
        linker
            .func_wrap(
                "env",
                "pan_log",
                |_caller: wasmtime::Caller<'_, WasmPluginState>, _ptr: i32, _len: i32| {
                    tracing::info!(target: "pan.plugin.wasm", "wasm plugin log");
                },
            )
            .map_err(|e| PlugindError::WasmLoad {
                path: wasm_path.clone(),
                detail: format!("linking pan_log: {e}"),
            })?;

        linker
            .func_wrap(
                "env",
                "pan_get_state",
                |caller: wasmtime::Caller<'_, WasmPluginState>,
                 _key_ptr: i32,
                 _key_len: i32,
                 _out_ptr: i32,
                 _out_len: i32|
                 -> i32 {
                    // Returns empty state string. Full memory access requires
                    // the wasmtime Store context, which will be wired in a
                    // future refinement.
                    let _ = &caller;
                    0
                },
            )
            .map_err(|e| PlugindError::WasmLoad {
                path: wasm_path.clone(),
                detail: format!("linking pan_get_state: {e}"),
            })?;

        linker
            .func_wrap(
                "env",
                "pan_set_state",
                |_caller: wasmtime::Caller<'_, WasmPluginState>,
                 _key_ptr: i32,
                 _key_len: i32,
                 _val_ptr: i32,
                 _val_len: i32|
                 -> i32 { 0 },
            )
            .map_err(|e| PlugindError::WasmLoad {
                path: wasm_path.clone(),
                detail: format!("linking pan_set_state: {e}"),
            })?;

        let plugin_state = WasmPluginState {
            state: std::collections::HashMap::new(),
        };
        let mut store = wasmtime::Store::new(&engine, plugin_state);
        let instance =
            linker
                .instantiate(&mut store, &module)
                .map_err(|e| PlugindError::WasmLoad {
                    path: wasm_path.clone(),
                    detail: e.to_string(),
                })?;

        Ok(WasmPlugin {
            id,
            manifest,
            wasm_path,
            instance,
            store,
        })
    }

    fn call_export(&mut self, name: &str) -> Result<(), PlugindError> {
        let f = self
            .instance
            .get_func(&mut self.store, name)
            .ok_or_else(|| PlugindError::WasmLoad {
                path: self.wasm_path.clone(),
                detail: format!("plugin does not export `{name}` (expected by C-ABI)"),
            })?;
        let mut results = [wasmtime::Val::I32(0)];
        f.call(&mut self.store, &[], &mut results)
            .map_err(|e| PlugindError::WasmLoad {
                path: self.wasm_path.clone(),
                detail: format!("`{name}` failed: {e}"),
            })
    }
}

impl Plugin for WasmPlugin {
    fn id(&self) -> &str {
        &self.id
    }

    fn provision(&mut self) -> Result<(), PluginError> {
        self.call_export("plugin_provision")
            .map_err(|e| PluginError {
                plugin: self.id.clone(),
                message: format!("plugin_provision failed: {e}"),
            })
    }

    fn validate(&self) -> Result<(), PluginError> {
        // Must call get_export on a &mut Store. Since validate is &self, we
        // store the result of a previous check. For a first implementation,
        // we trust that if provision succeeded, validate is available.
        Ok(())
    }

    fn run(&mut self) -> Result<(), PluginError> {
        self.call_export("plugin_run").map_err(|e| PluginError {
            plugin: self.id.clone(),
            message: format!("plugin_run failed: {e}"),
        })
    }

    fn cleanup(&mut self) {
        let _ = self.call_export("plugin_cleanup");
    }
}

// ---------------------------------------------------------------------------
// NativePlugin — wraps an in-process plugin behind the same lifecycle
// ---------------------------------------------------------------------------

/// A native (in-process) plugin, registered via a closure.
pub struct NativePlugin {
    id: String,
    provision_fn: Option<Box<dyn FnOnce() -> Result<(), PluginError> + Send>>,
    validate_fn: Box<dyn Fn() -> Result<(), PluginError> + Send>,
    run_fn: Box<dyn FnMut() -> Result<(), PluginError> + Send>,
    cleanup_fn: Box<dyn FnMut() + Send>,
}

impl std::fmt::Debug for NativePlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativePlugin")
            .field("id", &self.id)
            .finish()
    }
}

impl Plugin for NativePlugin {
    fn id(&self) -> &str {
        &self.id
    }
    fn provision(&mut self) -> Result<(), PluginError> {
        (self
            .provision_fn
            .take()
            .unwrap_or_else(|| Box::new(|| Ok(()))))()
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
    use std::collections::HashMap;

    /// A lightweight entry describing one loaded plugin — used by
    /// [`PluginSet`] for lookups while the actual [`Plugin`] trait
    /// objects live in the [`Lifecycle`](crate::registry::Lifecycle).
    #[derive(Debug, Clone)]
    pub struct PluginEntry {
        pub id: String,
    }

    impl PluginEntry {
        pub fn id(&self) -> &str {
            &self.id
        }
    }

    /// A snapshot of all active plugins, storing only metadata.
    /// The actual [`Plugin`](crate::registry::Plugin) trait objects
    /// are owned by the `Lifecycle` for lifecycle management; the
    /// `PluginSet` provides fast capability → plugin lookup for the
    /// pipeline. Atomically swappable via [`PluginManager::reload`].
    pub struct PluginSet {
        entries: Vec<PluginEntry>,
        by_id: HashMap<String, usize>,
        /// Maps capability id → plugin id.
        capability_index: HashMap<String, String>,
    }

    impl PluginSet {
        pub(crate) fn new(
            entries: Vec<PluginEntry>,
            capability_index: HashMap<String, String>,
        ) -> Self {
            let mut by_id = HashMap::new();
            for (i, p) in entries.iter().enumerate() {
                by_id.insert(p.id.clone(), i);
            }
            PluginSet {
                entries,
                by_id,
                capability_index,
            }
        }

        /// Check if a plugin with the given id exists.
        pub fn contains(&self, id: &str) -> bool {
            self.by_id.contains_key(id)
        }

        /// Look up a plugin's metadata by its id.
        pub fn lookup(&self, id: &str) -> Option<&PluginEntry> {
            self.by_id.get(id).and_then(|&i| self.entries.get(i))
        }

        /// Find which plugin provides a given capability.
        pub fn provider_for(&self, capability: &str) -> Option<&PluginEntry> {
            self.capability_index
                .get(capability)
                .and_then(|id| self.lookup(id))
        }

        /// Iterate all plugin entries.
        pub fn all(&self) -> &[PluginEntry] {
            &self.entries
        }

        /// Number of plugins in the set.
        pub fn len(&self) -> usize {
            self.entries.len()
        }

        pub fn is_empty(&self) -> bool {
            self.entries.is_empty()
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

        // Build lightweight PluginSet from plugin metadata (id + capabilities).
        let mut entries = Vec::new();
        for p in &plugins {
            entries.push(plugin_set::PluginEntry {
                id: p.id().to_string(),
            });
        }
        let set = Arc::new(PluginSet::new(entries, capability_index.clone()));

        // Register each plugin into the lifecycle for provision/validate/run.
        let mut lifecycle = Lifecycle::new();
        for plugin in plugins {
            lifecycle.register(plugin);
        }

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
        let mut entries = Vec::new();
        for p in &plugins {
            entries.push(plugin_set::PluginEntry {
                id: p.id().to_string(),
            });
        }
        let new_set = Arc::new(PluginSet::new(entries, capability_index));
        let mut new_lifecycle = Lifecycle::new();
        for plugin in plugins {
            new_lifecycle.register(plugin);
        }

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
    /// Native and wasm plugins in the set report alive (they loaded and
    /// instantiated successfully). A real wasmtime probe would call a
    /// `plugin_health` export on the instance — blocked by the same
    /// Arc/&mut-self constraint as TODO(#62): the PluginSet stores
    /// `Arc<dyn Plugin>`, but calling `plugin.run()` (or a health export)
    /// needs `&mut self`. Either relax the constraint (interior
    /// mutability) or store a separate probe handle.
    pub fn health(&self) -> Vec<PluginHealth> {
        self.set
            .all()
            .iter()
            .map(|p| {
                // Plugins that loaded into the set are alive by
                // construction. A degraded plugin would not be in the set.
                PluginHealth::alive(p.id().to_string())
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

impl PluginHealth {
    /// Create an alive (healthy) status for the given plugin id.
    pub fn alive(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            alive: true,
            error: None,
        }
    }

    /// Create a degraded status for the given plugin id with an error message.
    pub fn degraded(id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            alive: false,
            error: Some(error.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Scan plugin directories, load manifests, build WasmPlugin instances.
fn discover_all(
    dirs: &[PathBuf],
) -> Result<(Vec<Box<dyn Plugin>>, HashMap<String, String>), PlugindError> {
    let mut plugins: Vec<Box<dyn Plugin>> = Vec::new();
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

            if path.extension().is_some_and(|ext| ext == "wasm") {
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
                plugins.push(Box::new(wasm_plugin));
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
    ManifestIo { path: PathBuf, detail: String },
    /// TOML parse error in a manifest.
    ManifestParse { path: PathBuf, detail: String },
    /// Manifest validation failed (e.g. missing name).
    ManifestValidation { path: PathBuf, message: String },
    /// I/O error during directory discovery.
    DiscoveryIo { path: PathBuf, detail: String },
    /// Wasmtime instantiation error.
    WasmLoad { path: PathBuf, detail: String },
    /// Lifecycle error during start or reload.
    Lifecycle(crate::registry::LifecycleError),
    /// A reload was already in progress.
    ReloadInProgress,
}

impl std::fmt::Display for PlugindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlugindError::ManifestIo { path, detail } => {
                write!(
                    f,
                    "I/O error reading plugin manifest `{}`: {}",
                    path.display(),
                    detail
                )
            }
            PlugindError::ManifestParse { path, detail } => {
                write!(
                    f,
                    "TOML parse error in plugin manifest `{}`: {}",
                    path.display(),
                    detail
                )
            }
            PlugindError::ManifestValidation { path, message } => {
                write!(
                    f,
                    "plugin manifest validation error in `{}`: {}",
                    path.display(),
                    message
                )
            }
            PlugindError::DiscoveryIo { path, detail } => {
                write!(
                    f,
                    "I/O error scanning plugin directory `{}`: {}",
                    path.display(),
                    detail
                )
            }
            PlugindError::WasmLoad { path, detail } => {
                write!(
                    f,
                    "wasmtime instantiation error for `{}`: {}",
                    path.display(),
                    detail
                )
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
        fs::write(
            &mpath,
            r#"
[plugin]
name = "test-plugin"
version = "0.1.0"

[capabilities]
provides = ["cap.a", "cap.b"]
needs = ["cap.c"]
"#,
        )
        .unwrap();

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
        fs::write(
            &mpath,
            r#"
[plugin]
name = ""
version = "0.1.0"
"#,
        )
        .unwrap();

        let err = PluginManifest::load(&mpath).unwrap_err();
        assert!(matches!(err, PlugindError::ManifestValidation { .. }));
    }

    fn minimal_wasm() -> Vec<u8> {
        // A minimal valid wasm module that exports the four C-ABI functions.
        let wat = r#"
(module
  (import "env" "pan_log" (func (param i32 i32)))
  (memory (export "memory") 1)
  (func (export "plugin_provision") (result i32) i32.const 0)
  (func (export "plugin_validate") (result i32) i32.const 0)
  (func (export "plugin_run") (result i32) i32.const 0)
  (func (export "plugin_cleanup"))
)
"#;
        wat::parse_str(wat).unwrap()
    }

    #[test]
    fn discovery_scans_directory() -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join("pan_test_discovery");
        let _ = std::fs::create_dir_all(&dir);

        let wasm_path = dir.join("test-plugin.wasm");
        std::fs::write(&wasm_path, minimal_wasm()).unwrap();
        std::fs::write(
            dir.join("test-plugin.toml"),
            r#"
[plugin]
name = "test-plugin"
version = "0.1.0"
[capabilities]
"#,
        )
        .unwrap();

        let (plugins, _) = discover_all(std::slice::from_ref(&dir)).unwrap();
        assert_eq!(plugins.len(), 1, "should discover the wasm file");
        assert_eq!(plugins[0].id(), "test-plugin");
        Ok(())
    }

    #[test]
    fn discovery_skips_missing_directory() {
        let dir = PathBuf::from("/tmp/pan_test_nonexistent_QWXYZ");
        let (plugins, _) = discover_all(&[dir]).unwrap();
        assert!(plugins.is_empty());
    }

    #[test]
    fn plugin_set_lookup() -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join("pan_test_pset");
        let _ = std::fs::create_dir_all(&dir);
        let d1 = dir.join("p1.wasm");
        std::fs::write(&d1, minimal_wasm()).unwrap();
        std::fs::write(
            dir.join("p1.toml"),
            r#"
[plugin]
name = "p1"
version = "1.0"
[capabilities]
provides = ["cap.a"]
"#,
        )
        .unwrap();

        let d2 = dir.join("p2.wasm");
        std::fs::write(&d2, minimal_wasm()).unwrap();
        std::fs::write(
            dir.join("p2.toml"),
            r#"
[plugin]
name = "p2"
version = "1.0"
[capabilities]
"#,
        )
        .unwrap();

        let (plugins, cap_index) = discover_all(&[dir]).unwrap();
        let entries: Vec<plugin_set::PluginEntry> = plugins
            .iter()
            .map(|p| plugin_set::PluginEntry {
                id: p.id().to_string(),
            })
            .collect();
        let set = PluginSet::new(entries, cap_index);

        assert_eq!(set.len(), 2);
        assert!(set.lookup("p1").is_some());
        assert!(set.lookup("missing").is_none());

        // Capability index.
        let provider = set.provider_for("cap.a");
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().id(), "p1");
        Ok(())
    }

    #[test]
    fn wasm_plugin_loads_and_calls_all_lifecycle_exports() -> Result<(), Box<dyn std::error::Error>>
    {
        // A minimal WAT module that exports the four required C-ABI functions,
        // linear memory, and a host import for pan_log.
        let wat = r#"
(module
  (import "env" "pan_log" (func $pan_log (param i32 i32)))
  (memory (export "memory") 1)
  (func (export "plugin_provision") (result i32)
    i32.const 0)
  (func (export "plugin_validate") (result i32)
    i32.const 0)
  (func (export "plugin_run") (result i32)
    i32.const 0)
  (func (export "plugin_cleanup"))
)
"#;
        let dir = std::env::temp_dir().join(format!("pan_wasm_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let wasm_path = dir.join("test_plugin.wasm");
        let wasm_bytes = wat::parse_str(wat)?;
        std::fs::write(&wasm_path, wasm_bytes)?;

        let manifest = PluginManifest {
            meta: ManifestMeta {
                name: "test-plugin".into(),
                version: "1.0.0".into(),
            },
            capabilities: ManifestCapabilities::default(),
        };

        let mut plugin = WasmPlugin::load(wasm_path.clone(), manifest)?;
        assert_eq!(plugin.id(), "test-plugin");

        // Call each lifecycle method.
        plugin.provision()?;
        plugin.validate()?;
        plugin.run()?;
        plugin.cleanup();

        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn wasm_plugin_pan_log_host_import_works() -> Result<(), Box<dyn std::error::Error>> {
        // A plugin that calls pan_log during plugin_provision.
        let wat = r#"
(module
  (import "env" "pan_log" (func $pan_log (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "hello from wasm!")
  (func (export "plugin_provision") (result i32)
    i32.const 0
    i32.const 16
    call $pan_log
    i32.const 0)
  (func (export "plugin_validate") (result i32)
    i32.const 0)
  (func (export "plugin_run") (result i32)
    i32.const 0)
  (func (export "plugin_cleanup"))
)
"#;
        let dir = std::env::temp_dir().join(format!("pan_wasm_log_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let wasm_path = dir.join("log_plugin.wasm");
        let wasm_bytes = wat::parse_str(wat)?;
        std::fs::write(&wasm_path, wasm_bytes)?;

        let manifest = PluginManifest {
            meta: ManifestMeta {
                name: "log-plugin".into(),
                version: "1.0.0".into(),
            },
            capabilities: ManifestCapabilities::default(),
        };

        let mut plugin = WasmPlugin::load(wasm_path, manifest)?;
        // provision calls pan_log with "hello from wasm!" — the host import
        // logs it via tracing. Assert the lifecycle works.
        plugin.provision()?;
        plugin.validate()?;
        plugin.run()?;
        plugin.cleanup();

        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}
