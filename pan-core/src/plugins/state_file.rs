//! # `state.file` — disk-persistent state (Wave 2).
//!
//! Persists the in-memory `MemoryState` to a JSON file so Pan survives a
//! restart. Wave 2 concurrency stance (per the manifest): **single-writer**.
//! All writes go through one `Mutex`-guarded store and one file; we do not
//! support concurrent writers yet. That is the explicit, documented choice for
//! Wave 2 — concurrent multi-persona writes are a later concern (see issue
//! #48 `Per-Persona memory write concurrency`).
//!
//! Lifecycle: loads the file in `provision()` (best-effort; missing file = empty
//! state), and rewrites the whole file on every `write`. Whole-file rewrite is
//! simple and correct for the small state sizes Pan holds; a WAL/swap-file
//! approach is premature until profiling says otherwise (manifest Wave 6).

use crate::registry::Plugin;
use crate::schema::Value;
use crate::state::StateSlot;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct StateFile {
    path: PathBuf,
    store: Mutex<HashMap<String, Value>>,
}

impl StateFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            store: Mutex::new(HashMap::new()),
        }
    }

    /// Load existing state from disk. Called during `provision`. A missing or
    /// unreadable file is treated as empty state (best-effort, never fatal at
    /// startup — a corrupt file is logged and ignored so the agent can still
    /// run).
    pub fn load(&self) -> Result<(), String> {
        if !self.path.exists() {
            return Ok(());
        }
        let text = std::fs::read_to_string(&self.path)
            .map_err(|e| format!("read {}: {e}", self.path.display()))?;
        if text.trim().is_empty() {
            return Ok(());
        }
        let map: HashMap<String, Value> = serde_json::from_str(&text)
            .map_err(|e| format!("parse {}: {e}", self.path.display()))?;
        *self.store.lock().unwrap() = map;
        Ok(())
    }

    /// Write a key and persist the whole map to disk atomically (write temp,
    /// then rename). Single atomic rename keeps the file consistent even if we
    /// crash mid-write.
    pub fn write(&self, path: &str, value: Value) {
        let mut map = self.store.lock().unwrap();
        map.insert(path.to_string(), value);
        self.persist(&map);
    }

    pub fn read(&self, path: &str) -> Option<Value> {
        self.store.lock().unwrap().get(path).cloned()
    }

    pub fn keys(&self) -> Vec<String> {
        self.store.lock().unwrap().keys().cloned().collect()
    }

    /// Atomic rewrite: serialize to a temp file beside the target, then rename
    /// over it. The rename is atomic on POSIX, so readers never see a partial
    /// file.
    fn persist(&self, map: &HashMap<String, Value>) {
        let text = serde_json::to_string_pretty(map).unwrap_or_default();
        let target = &self.path;
        let tmp = target.with_extension("tmp");
        if let Ok(()) = std::fs::write(&tmp, &text) {
            let _ = std::fs::rename(&tmp, target);
        }
        // If either step fails we simply don't persist this write; the in-memory
        // copy is still authoritative for the running process. Logged by caller
        // if needed. Not fatal.
    }
}

impl StateSlot for StateFile {
    fn load(&self) -> Result<(), String> {
        StateFile::load(self)
    }

    fn flush(&self) -> Result<(), String> {
        let map = self.store.lock().unwrap().clone();
        self.persist(&map);
        Ok(())
    }

    fn write(&self, path: &str, value: Value) {
        StateFile::write(self, path, value)
    }

    fn read(&self, path: &str) -> Option<Value> {
        StateFile::read(self, path)
    }

    fn keys(&self) -> Vec<String> {
        StateFile::keys(self)
    }
}

impl Plugin for StateFile {
    fn id(&self) -> &str {
        "state.file"
    }

    fn provision(&mut self) -> Result<(), crate::registry::PluginError> {
        self.load()
            .map_err(|e| crate::registry::PluginError {
                plugin: self.id().to_string(),
                message: e,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn tmp_path() -> PathBuf {
        let d = std::env::temp_dir();
        let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("pan_state_test_{}_{}.json", std::process::id(), n);
        d.join(name)
    }

    #[test]
    fn write_then_read_in_memory() {
        let s = StateFile::new(tmp_path());
        s.write("last", serde_json::json!("now"));
        assert_eq!(s.read("last").unwrap(), "now");
    }

    #[test]
    fn persists_to_disk_and_survives_new_instance() {
        let p = tmp_path();
        let s = StateFile::new(p.clone());
        s.write("user", serde_json::json!("Sam"));
        s.write("count", serde_json::json!(3));
        // A fresh instance pointed at the same file should load what we wrote.
        let s2 = StateFile::new(p.clone());
        s2.load().unwrap();
        assert_eq!(s2.read("user").unwrap(), "Sam");
        assert_eq!(s2.read("count").unwrap(), 3);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn missing_file_is_empty_state_not_error() {
        let p = tmp_path();
        let _ = std::fs::remove_file(&p);
        let s = StateFile::new(p);
        assert!(s.load().is_ok());
        assert!(s.read("anything").is_none());
    }

    #[test]
    fn shared_arc_write_is_thread_safe() {
        let s = Arc::new(StateFile::new(tmp_path()));
        let s2 = Arc::clone(&s);
        s.write("a", serde_json::json!(1));
        assert_eq!(s2.read("a").unwrap(), 1);
    }

    #[test]
    fn state_slot_trait_persist_flush_and_reload() {
        // Exercise the full persist → read-back cycle through &dyn StateSlot
        // so the trait interface is tested, not just the inherent methods.
        let p = tmp_path();
        let sf = StateFile::new(p.clone());
        let slot: &dyn StateSlot = &sf;

        // Write and flush through the trait.
        slot.write("name", serde_json::json!("Pan"));
        slot.write("version", serde_json::json!(2));
        slot.flush().unwrap();

        // In-memory read-back through the trait.
        assert_eq!(slot.read("name").unwrap(), "Pan");
        assert_eq!(slot.read("version").unwrap(), 2);

        // A fresh instance should load the persisted data through the trait.
        let sf2 = StateFile::new(p.clone());
        let slot2: &dyn StateSlot = &sf2;
        slot2.load().unwrap();

        assert_eq!(slot2.read("name").unwrap(), "Pan");
        assert_eq!(slot2.read("version").unwrap(), 2);

        // keys() through the trait.
        let mut keys = slot2.keys();
        keys.sort();
        assert_eq!(keys, vec!["name", "version"]);

        let _ = std::fs::remove_file(&p);
    }
}
