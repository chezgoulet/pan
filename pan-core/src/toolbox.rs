//! # Capability components and the toolbox — the execute stage, composed.
//!
//! The pipeline's `resolve` stage needs a [`CapabilityRegistry`] (which
//! capabilities exist, and their arg schemas) and its `execute` stage needs an
//! [`Executor`] (how to run them). A real agent gets both from a set of
//! **capability components**: each declares the capabilities it provides and runs
//! them. `cap.fs`, `cap.http`, `cap.shell`, `cap.state` are each one such
//! component.
//!
//! [`CapabilityProvider`] is that unit, and [`Toolbox`] composes many of them: it
//! builds the merged registry the pipeline resolves against, and it *is* the
//! [`Executor`], routing each capability to the component that owns it. The
//! toolbox is the plan's `exec.local` — in-process capability dispatch.
//!
//! Nothing here performs real I/O: the concrete `cap.*` components live outside
//! the irreducible core (see the `pan-cap` crate). This module is the abstraction
//! and the multiplexer, sibling to the trivial `EchoExecutor` stub.

use std::collections::HashMap;

use crate::pipeline::{ExecError, Executor};
use crate::registry::{CapabilityRegistry, ConflictError};
use crate::schema::{Capability, Value};

/// A component that provides one or more capabilities: it declares them (id +
/// schema, so `resolve`/`validate` work) and executes them. The unit the plan
/// calls a tool / `cap.*` component.
#[async_trait::async_trait]
pub trait CapabilityProvider: Send + Sync {
    /// A stable component id, for diagnostics — e.g. `"cap.fs"`.
    fn id(&self) -> &str;

    /// The capabilities this component provides (id + args schema). Registered
    /// into the pipeline's [`CapabilityRegistry`] by the [`Toolbox`].
    fn capabilities(&self) -> Vec<Capability>;

    /// Execute one of this component's capabilities. `capability` is guaranteed
    /// to be one this component declared — the toolbox routes strictly by
    /// ownership, so a component never sees a capability it did not provide.
    async fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError>;
}

/// In-process capability dispatch — the plan's `exec.local`.
///
/// Holds a set of [`CapabilityProvider`]s, builds the merged
/// [`CapabilityRegistry`] the pipeline resolves against, and implements
/// [`Executor`] by routing each capability to the component that owns it. Two
/// components claiming the same capability id is a **conflict error, never
/// last-wins** — the same discipline the registry enforces.
#[derive(Default)]
pub struct Toolbox {
    providers: Vec<Box<dyn CapabilityProvider>>,
    owner: HashMap<String, usize>,
}

impl Toolbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a capability component. Errors (adding nothing) if any capability it
    /// provides collides with one already present.
    pub fn add(&mut self, provider: Box<dyn CapabilityProvider>) -> Result<(), ConflictError> {
        let caps = provider.capabilities();
        // Validate the whole component against the current set BEFORE mutating,
        // so a rejected add leaves the toolbox untouched.
        for cap in &caps {
            if self.owner.contains_key(&cap.id) {
                return Err(ConflictError { id: cap.id.clone() });
            }
        }
        let idx = self.providers.len();
        for cap in &caps {
            self.owner.insert(cap.id.clone(), idx);
        }
        self.providers.push(provider);
        Ok(())
    }

    /// Chainable [`add`](Self::add), for building a toolbox inline.
    pub fn with(mut self, provider: Box<dyn CapabilityProvider>) -> Result<Self, ConflictError> {
        self.add(provider)?;
        Ok(self)
    }

    /// Build the [`CapabilityRegistry`] of everything this toolbox provides — the
    /// menu `resolve` reads and the provider is offered. Uniqueness was enforced
    /// at [`add`](Self::add) time, so registration here cannot conflict.
    pub fn registry(&self) -> CapabilityRegistry {
        let mut reg = CapabilityRegistry::new();
        for provider in &self.providers {
            for cap in provider.capabilities() {
                let _ = reg.register(cap);
            }
        }
        reg
    }

    /// The capability ids this toolbox can run.
    pub fn capability_ids(&self) -> impl Iterator<Item = &str> {
        self.owner.keys().map(String::as_str)
    }
}

#[async_trait::async_trait]
impl Executor for Toolbox {
    fn id(&self) -> &str {
        "exec.local"
    }

    async fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError> {
        match self.owner.get(capability) {
            Some(&idx) => self.providers[idx].execute(capability, args).await,
            // The pipeline's `resolve` stage should have caught this first; if a
            // caller reaches execute with an unowned id, fail loudly rather than
            // silently.
            None => Err(ExecError(format!(
                "no capability component provides `{capability}`"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial component providing two capabilities that echo their args.
    struct EchoCaps;
    #[async_trait::async_trait]
    impl CapabilityProvider for EchoCaps {
        fn id(&self) -> &str {
            "cap.echo"
        }
        fn capabilities(&self) -> Vec<Capability> {
            vec![
                Capability {
                    id: "cap.echo.say".into(),
                    summary: "echo the args".into(),
                    args_schema: serde_json::json!({ "type": "object" }),
                },
                Capability {
                    id: "cap.echo.ping".into(),
                    summary: "return pong".into(),
                    args_schema: serde_json::json!({ "type": "object" }),
                },
            ]
        }
        async fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError> {
            Ok(serde_json::json!({ "ran": capability, "args": args }))
        }
    }

    /// A conflicting component that also claims `cap.echo.say`.
    struct Clasher;
    #[async_trait::async_trait]
    impl CapabilityProvider for Clasher {
        fn id(&self) -> &str {
            "cap.clash"
        }
        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability {
                id: "cap.echo.say".into(),
                summary: String::new(),
                args_schema: serde_json::json!({ "type": "object" }),
            }]
        }
        async fn execute(&self, _c: &str, _a: &Value) -> Result<Value, ExecError> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn registry_merges_all_provided_capabilities() {
        let tb = Toolbox::new().with(Box::new(EchoCaps)).unwrap();
        let reg = tb.registry();
        assert!(reg.lookup("cap.echo.say").is_some());
        assert!(reg.lookup("cap.echo.ping").is_some());
        assert!(reg.lookup("cap.missing").is_none());
    }

    #[tokio::test]
    async fn execute_routes_to_the_owning_component() {
        let tb = Toolbox::new().with(Box::new(EchoCaps)).unwrap();
        let out = tb
            .execute("cap.echo.ping", &serde_json::json!({ "n": 1 }))
            .await
            .unwrap();
        assert_eq!(out["ran"], "cap.echo.ping");
        assert_eq!(out["args"]["n"], 1);
    }

    #[tokio::test]
    async fn executing_an_unowned_capability_errors() {
        let tb = Toolbox::new().with(Box::new(EchoCaps)).unwrap();
        let err = tb.execute("cap.nope", &Value::Null).await.unwrap_err();
        assert!(err.0.contains("cap.nope"));
    }

    #[test]
    fn colliding_capability_ids_are_a_conflict_never_last_wins() {
        let mut tb = Toolbox::new().with(Box::new(EchoCaps)).unwrap();
        let err = tb.add(Box::new(Clasher)).unwrap_err();
        assert_eq!(err.id, "cap.echo.say");
        // The rejected add left the toolbox untouched.
        assert!(tb.registry().lookup("cap.echo.ping").is_some());
    }
}
