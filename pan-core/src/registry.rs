//! # Registry & plugin lifecycle.
//!
//! Two Wave-0 pieces live here:
//!
//! 1. [`CapabilityRegistry`] — where capabilities register; the pipeline's
//!    `resolve` stage reads from it. Registration by hierarchical id; a repeated
//!    id is a **conflict error, never last-wins** (synthesis §4 / build manifest).
//!
//! 2. [`Lifecycle`] — the Caddy-style plugin lifecycle
//!    `Register → Provision → Validate → Run → Cleanup`, driven over a set of
//!    plugins keyed by hierarchical id. Two plugins claiming the same id is a
//!    **Provision-time error**, not a silent override.
//!
//! Hierarchical ids (`provider.llm.anthropic`, `memory.vector.ragamuffin`) give
//! organization and conflict resolution; this module treats them as opaque
//! dotted strings and only enforces uniqueness.

use crate::schema::Capability;
use std::collections::BTreeMap;

/// Registry of invocable capabilities, read by the pipeline's `resolve` stage.
#[derive(Default)]
pub struct CapabilityRegistry {
    by_id: BTreeMap<String, Capability>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self { by_id: BTreeMap::new() }
    }

    /// Register a capability. Returns an error if its id is already taken —
    /// last-registration-wins is explicitly rejected, because a silent override
    /// is exactly the Caddy weakness the design set out to avoid.
    pub fn register(&mut self, cap: Capability) -> Result<(), ConflictError> {
        if self.by_id.contains_key(&cap.id) {
            return Err(ConflictError { id: cap.id });
        }
        self.by_id.insert(cap.id.clone(), cap);
        Ok(())
    }

    /// Resolve a capability id to its declaration. Used by `resolve`.
    pub fn lookup(&self, id: &str) -> Option<&Capability> {
        self.by_id.get(id)
    }

    /// All registered capabilities, e.g. to hand the provider the menu of verbs.
    pub fn all(&self) -> Vec<Capability> {
        self.by_id.values().cloned().collect()
    }

    pub fn len(&self) -> usize { self.by_id.len() }
    pub fn is_empty(&self) -> bool { self.by_id.is_empty() }
}

/// A name collision: two things claimed the same hierarchical id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictError {
    pub id: String,
}
impl std::fmt::Display for ConflictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "id `{}` is already registered (conflicts are errors, not last-wins)", self.id)
    }
}
impl std::error::Error for ConflictError {}

// ---------------------------------------------------------------------------
// Plugin lifecycle.
// ---------------------------------------------------------------------------

/// The Caddy-style plugin lifecycle. Every plugin is driven through the same
/// ordered phases. A plugin reports its hierarchical id; the [`Lifecycle`]
/// driver enforces id-uniqueness at provision time.
///
/// The phases beyond `id` have default no-op impls so a trivial plugin only
/// overrides what it needs. `provision` is where a plugin would receive its
/// granted handles (see [`crate::handles`]); `validate` is its self-check;
/// `run`/`cleanup` bracket its active life.
pub trait Plugin: Send {
    /// Hierarchical id, unique across the loaded set. Checked at provision time.
    fn id(&self) -> &str;

    /// Acquire dependencies / config. Errors here abort startup.
    fn provision(&mut self) -> Result<(), PluginError> { Ok(()) }

    /// Self-validate after provisioning; last chance to refuse before running.
    fn validate(&self) -> Result<(), PluginError> { Ok(()) }

    /// Enter active state. The loop runs between `run` and `cleanup`.
    fn run(&mut self) -> Result<(), PluginError> { Ok(()) }

    /// Release resources. Always attempted, even if an earlier phase failed.
    fn cleanup(&mut self) {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginError {
    pub plugin: String,
    pub message: String,
}
impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "plugin `{}`: {}", self.plugin, self.message)
    }
}
impl std::error::Error for PluginError {}

/// Drives a set of plugins through the lifecycle. Owns the plugins for their
/// active life and runs cleanup in reverse order on shutdown.
#[derive(Default)]
pub struct Lifecycle {
    plugins: Vec<Box<dyn Plugin>>,
}

impl Lifecycle {
    pub fn new() -> Self {
        Self { plugins: Vec::new() }
    }

    /// Register a plugin (phase 1). Insertion order is preserved; conflicts are
    /// caught at [`provision`](Self::provision), not here, so all registrations
    /// can be collected first and validated as a set.
    pub fn register(&mut self, plugin: Box<dyn Plugin>) {
        self.plugins.push(plugin);
    }

    /// Provision all plugins (phase 2), enforcing id-uniqueness across the whole
    /// set BEFORE provisioning any of them. Two plugins with the same id is a
    /// hard error — never last-wins.
    pub fn provision(&mut self) -> Result<(), LifecycleError> {
        let mut seen = std::collections::BTreeSet::new();
        for p in &self.plugins {
            if !seen.insert(p.id().to_string()) {
                return Err(LifecycleError::Conflict(ConflictError { id: p.id().to_string() }));
            }
        }
        for p in &mut self.plugins {
            p.provision().map_err(LifecycleError::Plugin)?;
        }
        Ok(())
    }

    /// Validate all plugins (phase 3).
    pub fn validate(&self) -> Result<(), LifecycleError> {
        for p in &self.plugins {
            p.validate().map_err(LifecycleError::Plugin)?;
        }
        Ok(())
    }

    /// Run all plugins (phase 4).
    pub fn run(&mut self) -> Result<(), LifecycleError> {
        for p in &mut self.plugins {
            p.run().map_err(LifecycleError::Plugin)?;
        }
        Ok(())
    }

    /// Clean up all plugins (phase 5), in reverse registration order. Cleanup is
    /// best-effort and never fails the shutdown.
    pub fn cleanup(&mut self) {
        for p in self.plugins.iter_mut().rev() {
            p.cleanup();
        }
    }

    /// The full startup sequence, returning to the caller in the "running" state.
    /// On any error, plugins provisioned so far are cleaned up before returning.
    pub fn start(&mut self) -> Result<(), LifecycleError> {
        if let Err(e) = self.provision().and_then(|_| self.validate()).and_then(|_| self.run()) {
            self.cleanup();
            return Err(e);
        }
        Ok(())
    }

    pub fn len(&self) -> usize { self.plugins.len() }
    pub fn is_empty(&self) -> bool { self.plugins.is_empty() }
}

#[derive(Debug)]
pub enum LifecycleError {
    Conflict(ConflictError),
    Plugin(PluginError),
}
impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LifecycleError::Conflict(e) => write!(f, "{e}"),
            LifecycleError::Plugin(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for LifecycleError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Capability;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::Arc;

    fn cap(id: &str) -> Capability {
        Capability { id: id.into(), summary: "".into(), args_schema: serde_json::json!({}) }
    }

    #[test]
    fn registry_rejects_duplicate_ids() {
        let mut r = CapabilityRegistry::new();
        r.register(cap("cap.fs.write")).unwrap();
        let err = r.register(cap("cap.fs.write")).unwrap_err();
        assert_eq!(err.id, "cap.fs.write");
        assert_eq!(r.len(), 1, "duplicate must not overwrite");
    }

    #[test]
    fn registry_resolves_and_lists() {
        let mut r = CapabilityRegistry::new();
        r.register(cap("a")).unwrap();
        r.register(cap("b")).unwrap();
        assert!(r.lookup("a").is_some());
        assert!(r.lookup("missing").is_none());
        assert_eq!(r.all().len(), 2);
    }

    // A plugin that records which lifecycle phases it saw, via a shared counter.
    struct Recorder {
        id: String,
        phases: Arc<AtomicU8>, // bitset: provision=1 validate=2 run=4 cleanup=8
    }
    const PROVISION: u8 = 1;
    const VALIDATE: u8 = 2;
    const RUN: u8 = 4;
    const CLEANUP: u8 = 8;
    impl Plugin for Recorder {
        fn id(&self) -> &str { &self.id }
        fn provision(&mut self) -> Result<(), PluginError> {
            self.phases.fetch_or(PROVISION, Ordering::SeqCst); Ok(())
        }
        fn validate(&self) -> Result<(), PluginError> {
            self.phases.fetch_or(VALIDATE, Ordering::SeqCst); Ok(())
        }
        fn run(&mut self) -> Result<(), PluginError> {
            self.phases.fetch_or(RUN, Ordering::SeqCst); Ok(())
        }
        fn cleanup(&mut self) {
            self.phases.fetch_or(CLEANUP, Ordering::SeqCst);
        }
    }

    #[test]
    fn lifecycle_runs_all_phases_in_order() {
        let phases = Arc::new(AtomicU8::new(0));
        let mut lc = Lifecycle::new();
        lc.register(Box::new(Recorder { id: "p.one".into(), phases: Arc::clone(&phases) }));
        lc.start().unwrap();
        lc.cleanup();
        assert_eq!(phases.load(Ordering::SeqCst), PROVISION | VALIDATE | RUN | CLEANUP);
    }

    #[test]
    fn lifecycle_rejects_duplicate_plugin_ids_at_provision() {
        let phases = Arc::new(AtomicU8::new(0));
        let mut lc = Lifecycle::new();
        lc.register(Box::new(Recorder { id: "dup".into(), phases: Arc::clone(&phases) }));
        lc.register(Box::new(Recorder { id: "dup".into(), phases: Arc::clone(&phases) }));
        let err = lc.provision().unwrap_err();
        assert!(matches!(err, LifecycleError::Conflict(_)));
        // No plugin should have been provisioned: the conflict is detected first.
        assert_eq!(phases.load(Ordering::SeqCst) & PROVISION, 0);
    }
}
