//! # `cap.state` — an in-process key/value store.
//!
//! The smallest useful capability component: a governed, in-memory KV store.
//! Provides `cap.state.set` and `cap.state.get`. It has no external dependency,
//! so it is the honest baseline for exercising the toolbox → pipeline path — and
//! it is the `state.memory` the plan lists.

use std::collections::HashMap;
use std::sync::Mutex;

use pan_core::pipeline::ExecError;
use pan_core::schema::{Capability, Value};
use pan_core::toolbox::CapabilityProvider;

/// An in-memory key/value store exposed as `cap.state.*`.
#[derive(Default)]
pub struct StateCaps {
    store: Mutex<HashMap<String, Value>>,
}

impl StateCaps {
    pub fn new() -> Self {
        Self::default()
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
                self.store.lock().unwrap().insert(key, value);
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
}
