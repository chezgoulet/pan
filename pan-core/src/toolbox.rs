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

use crate::invoker::ScopedInvoker;
use crate::pipeline::{ExecError, Executor};
use crate::registry::{CapabilityRegistry, ConflictError};
use crate::schema::{Capability, Value};

/// A component that provides one or more capabilities.
#[async_trait::async_trait]
pub trait CapabilityProvider: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> Vec<Capability>;
    async fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError>;

    /// Execute with a [`ScopedInvoker`]. Default delegates to execute.
    /// Capabilities like `cap.skill.run` override this.
    async fn execute_with_invoker(
        &self,
        capability: &str,
        args: &Value,
        _invoker: &dyn ScopedInvoker,
    ) -> Result<Value, ExecError> {
        self.execute(capability, args).await
    }
}

/// In-process capability dispatch.
#[derive(Default)]
pub struct Toolbox {
    providers: Vec<Box<dyn CapabilityProvider>>,
    owner: HashMap<String, usize>,
}

impl Toolbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, provider: Box<dyn CapabilityProvider>) -> Result<(), ConflictError> {
        let caps = provider.capabilities();
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

    pub fn with(mut self, provider: Box<dyn CapabilityProvider>) -> Result<Self, ConflictError> {
        self.add(provider)?;
        Ok(self)
    }

    pub fn registry(&self) -> CapabilityRegistry {
        let mut reg = CapabilityRegistry::new();
        for provider in &self.providers {
            for cap in provider.capabilities() {
                let _ = reg.register(cap);
            }
        }
        reg
    }

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
            None => Err(ExecError(format!(
                "no capability component provides `{capability}`"
            ))),
        }
    }

    async fn execute_with_invoker(
        &self,
        capability: &str,
        args: &Value,
        invoker: &dyn ScopedInvoker,
    ) -> Result<Value, ExecError> {
        match self.owner.get(capability) {
            Some(&idx) => {
                self.providers[idx]
                    .execute_with_invoker(capability, args, invoker)
                    .await
            }
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
