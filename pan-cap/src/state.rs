//! # `cap.state` — a key/value store.
//!
//! A governed KV store exposed as `cap.state.set` / `cap.state.get`. In-memory by
//! default (the `state.memory` the plan lists); given a file path it **persists**,
//! so `remember`/`recall` survive a restart (the plan's `state.file`). It is the
//! honest baseline for exercising the toolbox → pipeline path.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use pan_core::pipeline::ExecError;
use pan_core::schema::{Capability, Value};
use pan_core::toolbox::CapabilityProvider;

/// A key/value store exposed as `cap.state.*`. Backed by memory, or by a JSON
/// file when constructed with [`with_file`](Self::with_file).
#[derive(Default)]
pub struct StateCaps {
    store: Mutex<HashMap<String, Value>>,
    path: Option<PathBuf>,
}

impl StateCaps {
    /// An in-memory store (lost on restart).
    pub fn new() -> Self {
        Self::default()
    }

    /// A store persisted to `path` as JSON. If the file exists, its contents are
    /// loaded; every `set` rewrites it. A malformed or unreadable file starts
    /// empty rather than failing construction — the agent still runs.
    pub fn with_file(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let store = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            store: Mutex::new(store),
            path: Some(path),
        }
    }

    /// Persist a snapshot to disk (no-op for an in-memory store).
    fn persist(&self, snapshot: &HashMap<String, Value>) -> Result<(), ExecError> {
        if let Some(path) = &self.path {
            let json = serde_json::to_string_pretty(snapshot)
                .map_err(|e| ExecError(format!("serializing state: {e}")))?;
            std::fs::write(path, json).map_err(|e| ExecError(format!("persisting state: {e}")))?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl CapabilityProvider for StateCaps {
    fn id(&self) -> &str {
        "cap.state"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability {
                id: "cap.state.set".into(),
                summary: "store a value under a key".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "required": ["key", "value"]
                }),
            },
            Capability {
                id: "cap.state.get".into(),
                summary: "read the value stored under a key".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "required": ["key"]
                }),
            },
            Capability {
                id: "cap.state.list".into(),
                summary: "list all stored keys".into(),
                args_schema: serde_json::json!({ "type": "object" }),
            },
            Capability {
                id: "cap.state.delete".into(),
                summary: "delete a key from the store".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "required": ["key"]
                }),
            },
            Capability {
                id: "cap.state.namespaces".into(),
                summary: "list unique name prefixes (the part before the first `.`)".into(),
                args_schema: serde_json::json!({ "type": "object" }),
            },
        ]
    }

    async fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError> {
        let key = args
            .get("key")
            .and_then(|k| k.as_str())
            .ok_or_else(|| ExecError("`key` must be a string".into()))?
            .to_string();

        match capability {
            "cap.state.set" => {
                let value = args.get("value").cloned().unwrap_or(Value::Null);
                // Snapshot under the lock, persist outside it (no file I/O while
                // holding the mutex).
                let snapshot = {
                    let mut store = self.store.lock().unwrap();
                    store.insert(key, value);
                    self.path.as_ref().map(|_| store.clone())
                };
                if let Some(snapshot) = snapshot {
                    self.persist(&snapshot)?;
                }
                Ok(serde_json::json!({ "ok": true }))
            }
            "cap.state.get" => {
                let value = self
                    .store
                    .lock()
                    .unwrap()
                    .get(&key)
                    .cloned()
                    .unwrap_or(Value::Null);
                Ok(serde_json::json!({ "value": value }))
            }
            "cap.state.list" => {
                let keys: Vec<String> = self.store.lock().unwrap().keys().cloned().collect();
                Ok(serde_json::json!({ "keys": keys }))
            }
            "cap.state.delete" => {
                let snapshot = {
                    let mut store = self.store.lock().unwrap();
                    store.remove(&key);
                    self.path.as_ref().map(|_| store.clone())
                };
                if let Some(snapshot) = snapshot {
                    self.persist(&snapshot)?;
                }
                Ok(serde_json::json!({ "ok": true }))
            }
            "cap.state.namespaces" => {
                let store = self.store.lock().unwrap();
                let mut namespaces: Vec<String> = store
                    .keys()
                    .filter_map(|k| k.split('.').next())
                    .map(|n| n.to_string())
                    .collect();
                namespaces.sort();
                namespaces.dedup();
                Ok(serde_json::json!({ "namespaces": namespaces }))
            }
            other => Err(ExecError(format!("cap.state has no `{other}`"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_then_get_round_trips() {
        let s = StateCaps::new();
        s.execute(
            "cap.state.set",
            &serde_json::json!({ "key": "name", "value": "Sam" }),
        )
        .await
        .unwrap();
        let got = s
            .execute("cap.state.get", &serde_json::json!({ "key": "name" }))
            .await
            .unwrap();
        assert_eq!(got["value"], "Sam");
    }

    #[tokio::test]
    async fn missing_key_reads_null() {
        let s = StateCaps::new();
        let got = s
            .execute("cap.state.get", &serde_json::json!({ "key": "absent" }))
            .await
            .unwrap();
        assert_eq!(got["value"], Value::Null);
    }

    #[tokio::test]
    async fn a_file_backed_store_survives_a_restart() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "pan_state_{}_{}.json",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));

        // First "process": remember something, then drop the store.
        {
            let s = StateCaps::with_file(&path);
            s.execute(
                "cap.state.set",
                &serde_json::json!({ "key": "project", "value": "pan" }),
            )
            .await
            .unwrap();
        }

        // Second "process": a fresh store over the same file recalls it.
        let s = StateCaps::with_file(&path);
        let got = s
            .execute("cap.state.get", &serde_json::json!({ "key": "project" }))
            .await
            .unwrap();
        assert_eq!(got["value"], "pan", "state must persist across restarts");

        let _ = std::fs::remove_file(&path);
    }
}
