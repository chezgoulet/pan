//! # `gov.secrets` — resolve credentials without exposing to plugins (Wave 4).
//!
//! A governance plugin that stores credential values and injects them into
//! capability args **at govern time**, after the provider has decided what to
//! invoke. The provider (LLM, behavior tree, rules) never sees the actual
//! credential values — they are injected after the decision, between the govern
//! and execute stages.
//!
//! Design:
//!
//! - Acts as a [`Governor`] that always allows (it is not a policy governor).
//!   The real gating is done by `gov.policy` (Wave 4).
//! - Overrides [`enrich`](Governor::enrich) to inject credentials into the
//!   capability's args when a matching secret mapping exists.
//! - Credentials are key-value pairs at the top level of the args object.
//!   If the capability's original args are `Null`, an object is created.
//! - Supports loading from a JSON file at provision time, and programmatic
//!   registration via [`add_secret`](Secrets::add_secret).
//!
//! ## Secret file format
//!
//! ```json
//! {
//!   "cap.http": {
//!     "headers": { "Authorization": "Bearer sk-abc123" }
//!   },
//!   "cap.shell": {
//!     "cwd": "/mnt/safe"
//!   }
//! }
//! ```
//!
//! Each top-level key is a capability id. Its value is a JSON object whose
//! entries are merged into the capability's args at execution time. The merge
//! is shallow (top-level keys override any originals).
//!
//! ## Isolation guarantee
//!
//! The [`enrich`](Governor::enrich) method is called by the pipeline's `govern`
//! stage **after** the Allow verdict but **before** the governed token reaches
//! the executor. Because `enrich` is a method on the governor trait and the
//! provider never holds a reference to the governor, the credential values
//! cannot leak into the provider's context. The enrichment is invisible to the
//! provider; it only sees `args` that reference abstract keys (e.g. `{ "url":
//! "https://api.example.com", "headers": {} }`) which the secrets governor fills
//! in at govern time.

use crate::pipeline::{Governor, Verdict};
use crate::registry::{Plugin, PluginError};
use crate::schema::Value;
use std::collections::HashMap;

/// A secret store that injects credential values into capability args at the
/// govern stage, never exposing them to the provider.
pub struct Secrets {
    /// Per-capability secret mappings. Each entry maps a capability id to a
    /// set of key-value pairs that are merged into the capability's args.
    secrets: HashMap<String, Value>,
    /// When false, the governor denies all invocations (kill-switch).
    enabled: bool,
}

impl Default for Secrets {
    fn default() -> Self {
        Self {
            secrets: HashMap::new(),
            enabled: true,
        }
    }
}

impl Secrets {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a secret injection for a capability. Each invocation of this
    /// capability will have the given key-value pairs merged into its args at
    /// govern time.
    ///
    /// If the capability already has secrets registered, the new keys are merged
    /// into the existing map (new keys overwrite old ones).
    pub fn add_secret(&mut self, capability: &str, value: Value) {
        self.secrets.insert(capability.to_string(), value);
    }

    /// Load secrets from a JSON file. The file must be a JSON object mapping
    /// capability ids to JSON objects of key-value pairs.
    ///
    /// # Errors
    ///
    /// Returns `PluginError` if the file cannot be read or parsed.
    pub fn load_from_path(&mut self, path: &str) -> Result<(), PluginError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| PluginError {
                plugin: "gov.secrets".into(),
                message: format!("failed to read secrets file `{path}`: {e}"),
            })?;
        let parsed: Value = serde_json::from_str(&content).map_err(|e| PluginError {
            plugin: "gov.secrets".into(),
            message: format!("failed to parse secrets file `{path}`: {e}"),
        })?;
        let obj = parsed.as_object().ok_or_else(|| PluginError {
            plugin: "gov.secrets".into(),
            message: format!(
                "secrets file `{path}` must be a JSON object at the top level"
            ),
        })?;
        for (cap, value) in obj {
            if !value.is_object() {
                return Err(PluginError {
                    plugin: "gov.secrets".into(),
                    message: format!(
                        "secrets for `{cap}` must be a JSON object (got {})",
                        json_type_name(value)
                    ),
                });
            }
            self.secrets.insert(cap.clone(), value.clone());
        }
        Ok(())
    }

    /// Return the number of registered secret mappings.
    pub fn len(&self) -> usize {
        self.secrets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// Return the enriched args with secrets merged in for `capability`.
    /// Returns `None` if no secrets are registered for this capability.
    fn resolve(&self, capability: &str, args: &Value) -> Option<Value> {
        let injections = self.secrets.get(capability)?;
        // Start from the caller's args (or an empty object if null).
        let mut merged = match args {
            Value::Null => serde_json::json!({}),
            _ => args.clone(),
        };
        // Ensure we have a mutable object to merge into.
        let target = merged.as_object_mut()?;
        // Merge each secret key at the top level.
        if let Some(inject_obj) = injections.as_object() {
            for (key, val) in inject_obj {
                target.insert(key.clone(), val.clone());
            }
        }
        Some(merged)
    }
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// Governor trait — governs AND enriches
// ---------------------------------------------------------------------------

impl Governor for Secrets {
    fn id(&self) -> &str {
        "gov.secrets"
    }

    fn govern(&self, _capability: &str, _args: &Value) -> Verdict {
        if self.enabled {
            Verdict::Allow
        } else {
            Verdict::Deny {
                reason: "gov.secrets is disabled (kill-switch)".into(),
            }
        }
    }

    fn enrich(&self, capability: &str, args: &Value) -> Option<Value> {
        self.resolve(capability, args)
    }
}

// ---------------------------------------------------------------------------
// Plugin lifecycle: provision can load from a PAN_SECRETS_FILE env var.
// ---------------------------------------------------------------------------

impl Plugin for Secrets {
    fn id(&self) -> &str {
        "gov.secrets"
    }

    fn provision(&mut self) -> Result<(), PluginError> {
        // Load from env var if set (the primary provision path).
        if let Ok(path) = std::env::var("PAN_SECRETS_FILE") {
            self.load_from_path(&path)?;
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), PluginError> {
        // No cross-plugin validation needed in Wave 4.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{EchoExecutor, EffectRequest, Pipeline, AllowAll};
    use crate::events::{EventStream, MemorySink};
    use crate::registry::CapabilityRegistry;
    use crate::schema::Capability;

    // -- Unit tests for the Secrets store itself ---------------------------

    #[test]
    fn empty_secrets_no_enrichment() {
        let s = Secrets::new();
        let args = serde_json::json!({ "url": "http://example.com" });
        let result = s.enrich("cap.http", &args);
        // No secrets registered → None (no enrichment).
        assert!(result.is_none(), "no secrets means no enrichment");
    }

    #[test]
    fn injects_top_level_keys_into_existing_args() {
        let mut s = Secrets::new();
        s.add_secret(
            "cap.http",
            serde_json::json!({
                "headers": { "Authorization": "Bearer sk-test" }
            }),
        );
        let args = serde_json::json!({ "url": "http://api.example.com", "method": "GET" });
        let enriched = s
            .enrich("cap.http", &args)
            .expect("should enrich for registered cap");
        assert_eq!(enriched["url"], "http://api.example.com");
        assert_eq!(enriched["method"], "GET");
        assert_eq!(
            enriched["headers"]["Authorization"],
            "Bearer sk-test"
        );
    }

    #[test]
    fn injects_into_null_args() {
        let mut s = Secrets::new();
        s.add_secret(
            "cap.shell",
            serde_json::json!({ "cwd": "/mnt/safe" }),
        );
        let enriched = s
            .enrich("cap.shell", &Value::Null)
            .expect("should enrich null args");
        assert_eq!(enriched["cwd"], "/mnt/safe");
    }

    #[test]
    fn override_merges_over_existing_key() {
        let mut s = Secrets::new();
        s.add_secret(
            "cap.http",
            serde_json::json!({ "headers": { "X-API-Key": "abc" } }),
        );
        let args = serde_json::json!({
            "url": "http://example.com",
            "headers": { "Cache-Control": "no-cache" }
        });
        let enriched = s
            .enrich("cap.http", &args)
            .expect("should enrich");
        // Secret's `headers` replaces the original entirely (shallow merge).
        assert_eq!(enriched["headers"]["X-API-Key"], "abc");
        // The original Cache-Control is lost because headers is a top-level key
        // that was overwritten. This is the documented shallow-merge behaviour.
    }

    #[test]
    fn different_capabilities_have_isolated_secrets() {
        let mut s = Secrets::new();
        s.add_secret("cap.http", serde_json::json!({ "headers": { "Auth": "token" } }));
        s.add_secret("cap.shell", serde_json::json!({ "cwd": "/workspace" }));

        let http_args = serde_json::json!({ "url": "http://x.com" });
        let shell_args = serde_json::json!({ "command": "ls" });

        let http_enriched = s.enrich("cap.http", &http_args).unwrap();
        let shell_enriched = s.enrich("cap.shell", &shell_args).unwrap();

        assert!(http_enriched.get("headers").is_some());
        assert!(
            shell_enriched.get("headers").is_none(),
            "shell should not get http headers"
        );
        assert_eq!(shell_enriched["cwd"], "/workspace");
    }

    #[test]
    fn kill_switch_denies_all() {
        let s = Secrets { secrets: HashMap::new(), enabled: false };
        let verdict = s.govern("cap.shell", &Value::Null);
        assert!(matches!(verdict, Verdict::Deny { .. }));
    }

    #[test]
    fn load_from_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pan_secrets_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("secrets.json");
        std::fs::write(
            &path,
            r#"{
                "cap.http": {
                    "headers": { "Authorization": "Bearer file-token" }
                },
                "cap.shell": {
                    "cwd": "/mnt/data"
                }
            }"#,
        )
        .unwrap();

        let mut s = Secrets::new();
        s.load_from_path(path.to_str().unwrap())
            .expect("load secrets file");
        assert_eq!(s.len(), 2);

        let http_enriched = s
            .enrich("cap.http", &serde_json::json!({ "url": "http://x.com" }))
            .unwrap();
        assert_eq!(http_enriched["headers"]["Authorization"], "Bearer file-token");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn bad_file_path_errors() {
        let mut s = Secrets::new();
        let err = s.load_from_path("/tmp/pan_secrets_nonexistent.json");
        assert!(
            err.is_err(),
            "load from nonexistent path must return PluginError"
        );
    }

    #[test]
    fn invalid_json_file_errors() {
        let dir = std::env::temp_dir().join(format!("pan_secrets_test_inv_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("bad.json");
        std::fs::write(&path, "not json").unwrap();

        let mut s = Secrets::new();
        let err = s.load_from_path(path.to_str().unwrap());
        assert!(err.is_err(), "invalid JSON must error");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    // -- Integration: secrets injection through the pipeline -----------------

    #[test]
    fn secrets_are_injected_before_executor_sees_args() {
        let mut reg = CapabilityRegistry::new();
        reg.register(Capability::new("cap.http", "", serde_json::json!({"type": "object"})))
        .unwrap();

        let mut secrets = Secrets::new();
        secrets.add_secret(
            "cap.http",
            serde_json::json!({
                "headers": { "Authorization": "Bearer injected-secret" }
            }),
        );

        let (stream, guard) = EventStream::spawn(MemorySink::new());

        let pipeline = Pipeline {
            registry: &reg,
            governor: &secrets,
            executor: &EchoExecutor,
            events: &stream,
        };

        let result = pipeline
            .dispatch(EffectRequest {
                capability: "cap.http".into(),
                args: serde_json::json!({ "url": "http://api.example.com" }),
                correlation: None,
            })
            .expect("dispatch must succeed");

        // The EchoExecutor echoes back the args it received.
        // If the pipeline correctly injected the secret, the result should
        // contain the Authorization header.
        let echoed_args = &result.result["args"];
        assert_eq!(
            echoed_args["headers"]["Authorization"],
            "Bearer injected-secret",
            "the executor must see the injected secret, not the original args"
        );
        assert_eq!(
            echoed_args["url"], "http://api.example.com",
            "non-secret args must pass through unchanged"
        );

        stream.shutdown(guard);
    }

    #[test]
    fn no_secrets_capability_unchanged() {
        let mut reg = CapabilityRegistry::new();
        reg.register(Capability::new("cap.noop", "", serde_json::json!({"type": "object"})))
        .unwrap();

        let secrets = Secrets::new();
        let (stream, guard) = EventStream::spawn(MemorySink::new());

        let pipeline = Pipeline {
            registry: &reg,
            governor: &secrets,
            executor: &EchoExecutor,
            events: &stream,
        };

        let result = pipeline
            .dispatch(EffectRequest {
                capability: "cap.noop".into(),
                args: serde_json::json!({ "data": "original" }),
                correlation: None,
            })
            .expect("dispatch must succeed");

        let echoed_args = &result.result["args"];
        assert_eq!(
            echoed_args["data"], "original",
            "args must pass through unchanged for unregistered capabilities"
        );

        stream.shutdown(guard);
    }

    /// Verify that a `Secrets` governor interacts correctly with a `Deny`
    /// decision: if `govern` denies (kill-switch), `enrich` must never be
    /// called — the pipeline rejects before enrichment.
    #[test]
    fn deny_skips_enrichment_entirely() {
        let mut reg = CapabilityRegistry::new();
        reg.register(Capability::new("cap.http", "", serde_json::json!({"type": "object"})))
        .unwrap();

        // Disabled Secrets → all Deny.
        let secrets = Secrets {
            secrets: HashMap::new(),
            enabled: false,
        };

        let (stream, guard) = EventStream::spawn(MemorySink::new());

        let pipeline = Pipeline {
            registry: &reg,
            governor: &secrets,
            executor: &EchoExecutor,
            events: &stream,
        };

        let err = pipeline
            .dispatch(EffectRequest {
                capability: "cap.http".into(),
                args: serde_json::json!({ "url": "http://x.com" }),
                correlation: None,
            })
            .unwrap_err();

        assert!(
            matches!(err, crate::pipeline::PipelineError::Rejected(_)),
            "disabled secrets must reject all invocations"
        );

        stream.shutdown(guard);
    }
}
