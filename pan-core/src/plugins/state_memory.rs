//! # `state.memory` — in-process, non-persistent state (Wave 1).
//!
//! Gives `observe`/`commit` a slot: a tiny key/value store behind a `Plugin`,
//! so the walking skeleton has somewhere to put state writes (e.g. the
//! `cap.state_write` Invoke the stub LLM emits). Persists for the process
//! lifetime only; Wave 2's `state.file` adds disk durability. Single-writer
//! (a `Mutex`) is fine for Wave 1 — the manifest explicitly defers concurrency
//! stance to Wave 2.
//!
//! This is NOT a capability; it is a resource the loop/context can hold. The
//! `cap.state_write` *capability* is what providers invoke; an executor (or the
//! host wiring) routes that Invoke here. For the CLI skeleton we wire a small
//! executor closure that calls `MemoryState::write`.

use crate::registry::Plugin;
use crate::schema::Value;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct MemoryState {
    store: Mutex<HashMap<String, Value>>,
}

impl MemoryState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write(&self, path: &str, value: Value) {
        self.store.lock().unwrap().insert(path.to_string(), value);
    }

    pub fn read(&self, path: &str) -> Option<Value> {
        self.store.lock().unwrap().get(path).cloned()
    }

    pub fn keys(&self) -> Vec<String> {
        self.store.lock().unwrap().keys().cloned().collect()
    }
}

impl Plugin for MemoryState {
    fn id(&self) -> &str {
        "state.memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read() {
        let s = MemoryState::new();
        s.write("last_seen", serde_json::json!("now"));
        assert_eq!(s.read("last_seen").unwrap(), "now");
    }

    #[test]
    fn missing_key_is_none() {
        let s = MemoryState::new();
        assert!(s.read("nope").is_none());
    }

    #[test]
    fn overwrite_replaces() {
        let s = MemoryState::new();
        s.write("x", serde_json::json!(1));
        s.write("x", serde_json::json!(2));
        assert_eq!(s.read("x").unwrap(), 2);
    }
}
