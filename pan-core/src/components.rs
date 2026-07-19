//! # The component registry — config-driven wiring (ADR 0001, D3).
//!
//! Two extension mechanisms coexist in Pan and must not be confused:
//!
//! - **Plugin** — the out-of-process Wasm/`plugind` mechanism
//!   ([`crate::registry::Plugin`], `provision/validate/run/cleanup`). Orthogonal,
//!   later.
//! - **Component** — an *in-process* trait impl in one of the core families
//!   ([`Provider`], [`Governor`], [`Executor`], and, in later phases, channels /
//!   context sources / scheduler conditions), selected and wired by `Agent.toml`.
//!
//! This module is the backbone for the second: a set of **factories** keyed by a
//! config id. `Agent.toml` names a component (`provider = "provider.rules"`); the
//! registry maps that id to a constructor and hands it the component's settings
//! slice. This is what retires the hard-coded `RulesProvider` / `AllowAll` /
//! `EchoExecutor` wiring in the daemon's session — the graph becomes data.
//!
//! Consistent with [`crate::registry::CapabilityRegistry`], **registering two
//! factories under one id is an error, never last-wins**: a silent override of a
//! component constructor is exactly the ambiguity the design set out to avoid.

use std::collections::HashMap;

use crate::pipeline::{Executor, Governor};
use crate::schema::{Provider, Value};

/// The configuration slice handed to a component factory: the id it was named by
/// in `Agent.toml`, plus that component's own settings table (already converted
/// from TOML into the core [`Value`]).
#[derive(Debug, Clone, Default)]
pub struct ComponentConfig {
    pub id: String,
    pub settings: Value,
}

impl ComponentConfig {
    pub fn new(id: impl Into<String>, settings: Value) -> Self {
        Self {
            id: id.into(),
            settings,
        }
    }

    /// A config with no settings — for components that take none.
    pub fn bare(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            settings: Value::Null,
        }
    }
}

/// What went wrong building a component graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentError {
    /// No factory is registered for this id in this family.
    Unknown { family: &'static str, id: String },
    /// Two factories were registered under the same id in one family.
    Conflict { family: &'static str, id: String },
    /// A factory ran but refused to build (bad settings, unreachable dependency).
    Construction { id: String, reason: String },
}

impl std::fmt::Display for ComponentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComponentError::Unknown { family, id } => {
                write!(f, "no {family} component registered under id `{id}`")
            }
            ComponentError::Conflict { family, id } => write!(
                f,
                "{family} id `{id}` is already registered (conflicts are errors, not last-wins)"
            ),
            ComponentError::Construction { id, reason } => {
                write!(f, "component `{id}` failed to build: {reason}")
            }
        }
    }
}

impl std::error::Error for ComponentError {}

/// A factory constructs one component from its config. Boxed, `Send + Sync` so a
/// registry can be shared across the threads that assemble per-connection graphs.
type Factory<T> = Box<dyn Fn(&ComponentConfig) -> Result<Box<T>, ComponentError> + Send + Sync>;

/// Registry of component factories, one table per trait family. Populated once at
/// startup (a binary registers the components it was built with); read whenever a
/// persona / session needs to instantiate its configured graph.
#[derive(Default)]
pub struct ComponentRegistry {
    providers: HashMap<String, Factory<dyn Provider>>,
    governors: HashMap<String, Factory<dyn Governor>>,
    executors: HashMap<String, Factory<dyn Executor>>,
}

impl ComponentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    // --- registration (conflict = hard error) -----------------------------

    /// Register a [`Provider`] factory under `id`.
    pub fn register_provider<F>(
        &mut self,
        id: impl Into<String>,
        factory: F,
    ) -> Result<(), ComponentError>
    where
        F: Fn(&ComponentConfig) -> Result<Box<dyn Provider>, ComponentError>
            + Send
            + Sync
            + 'static,
    {
        insert_unique(
            "provider",
            &mut self.providers,
            id.into(),
            Box::new(factory),
        )
    }

    /// Register a [`Governor`] factory under `id`.
    pub fn register_governor<F>(
        &mut self,
        id: impl Into<String>,
        factory: F,
    ) -> Result<(), ComponentError>
    where
        F: Fn(&ComponentConfig) -> Result<Box<dyn Governor>, ComponentError>
            + Send
            + Sync
            + 'static,
    {
        insert_unique(
            "governor",
            &mut self.governors,
            id.into(),
            Box::new(factory),
        )
    }

    /// Register an [`Executor`] factory under `id`.
    pub fn register_executor<F>(
        &mut self,
        id: impl Into<String>,
        factory: F,
    ) -> Result<(), ComponentError>
    where
        F: Fn(&ComponentConfig) -> Result<Box<dyn Executor>, ComponentError>
            + Send
            + Sync
            + 'static,
    {
        insert_unique(
            "executor",
            &mut self.executors,
            id.into(),
            Box::new(factory),
        )
    }

    // --- construction ------------------------------------------------------

    /// Build the [`Provider`] configured under `cfg.id`.
    pub fn build_provider(
        &self,
        cfg: &ComponentConfig,
    ) -> Result<Box<dyn Provider>, ComponentError> {
        build("provider", &self.providers, cfg)
    }

    /// Build the [`Governor`] configured under `cfg.id`.
    pub fn build_governor(
        &self,
        cfg: &ComponentConfig,
    ) -> Result<Box<dyn Governor>, ComponentError> {
        build("governor", &self.governors, cfg)
    }

    /// Build the [`Executor`] configured under `cfg.id`.
    pub fn build_executor(
        &self,
        cfg: &ComponentConfig,
    ) -> Result<Box<dyn Executor>, ComponentError> {
        build("executor", &self.executors, cfg)
    }

    /// The provider ids this binary knows how to build — for diagnostics and to
    /// validate an `Agent.toml` before instantiating anything.
    pub fn provider_ids(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }
}

fn insert_unique<T: ?Sized>(
    family: &'static str,
    table: &mut HashMap<String, Factory<T>>,
    id: String,
    factory: Factory<T>,
) -> Result<(), ComponentError> {
    if table.contains_key(&id) {
        return Err(ComponentError::Conflict { family, id });
    }
    table.insert(id, factory);
    Ok(())
}

fn build<T: ?Sized>(
    family: &'static str,
    table: &HashMap<String, Factory<T>>,
    cfg: &ComponentConfig,
) -> Result<Box<T>, ComponentError> {
    match table.get(&cfg.id) {
        Some(factory) => factory(cfg),
        None => Err(ComponentError::Unknown {
            family,
            id: cfg.id.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::ScopedGovernor;
    use crate::providers::behaviortree::{BehaviorTreeProvider, Node};
    use crate::providers::rules::{Rule, RulesProvider};
    use crate::schema::{Context, Goal, Trigger};

    /// A provider factory that reads its rule from settings — the shape of a real
    /// `Agent.toml`-driven build, where nothing about the concrete type leaks
    /// past the registry boundary.
    fn register_stdlib(reg: &mut ComponentRegistry) {
        reg.register_provider("provider.rules", |cfg| {
            // A real factory parses cfg.settings; here we key off a tiny field.
            let topic = cfg
                .settings
                .get("when_event_topic")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ComponentError::Construction {
                    id: cfg.id.clone(),
                    reason: "missing `when_event_topic`".into(),
                })?;
            Ok(Box::new(RulesProvider {
                rules: vec![Rule {
                    when_signal_over: None,
                    when_event_topic: Some(topic.to_string()),
                    then_invoke: ("cap.noop".into(), Value::Null),
                }],
            }))
        })
        .unwrap();
        reg.register_provider("provider.behaviortree", |_cfg| {
            Ok(Box::new(BehaviorTreeProvider {
                root: vec![Node::Succeed],
            }))
        })
        .unwrap();
        reg.register_governor("gov.scoped", |_cfg| Ok(Box::new(ScopedGovernor::new())))
            .unwrap();
    }

    #[tokio::test]
    async fn builds_the_configured_provider_without_leaking_its_type() {
        let mut reg = ComponentRegistry::new();
        register_stdlib(&mut reg);

        // Agent.toml said `provider = "provider.rules"` with these settings.
        let cfg = ComponentConfig::new(
            "provider.rules",
            serde_json::json!({ "when_event_topic": "combat.started" }),
        );
        let provider: Box<dyn Provider> = reg.build_provider(&cfg).unwrap();

        // The caller only ever sees `dyn Provider`. Drive it to prove it's wired.
        let goal = Goal {
            id: "g".into(),
            revision: 0,
            objective: "react".into(),
            trigger: Trigger::Event {
                topic: "combat.started".into(),
                payload: Value::Null,
            },
        };
        let decision = provider.decide(&goal, &Context::default(), &[]).await;
        assert!(!decision.intents.is_empty(), "the built rule should fire");
    }

    #[test]
    fn unknown_component_is_an_error_not_a_panic() {
        let reg = ComponentRegistry::new();
        // `dyn Provider` isn't Debug, so match the Result rather than unwrap_err.
        let result = reg.build_provider(&ComponentConfig::bare("provider.nonexistent"));
        assert!(matches!(
            result,
            Err(ComponentError::Unknown {
                family: "provider",
                ..
            })
        ));
    }

    #[test]
    fn duplicate_registration_is_a_conflict_never_last_wins() {
        let mut reg = ComponentRegistry::new();
        reg.register_governor("gov.x", |_| Ok(Box::new(ScopedGovernor::new())))
            .unwrap();
        let err = reg
            .register_governor("gov.x", |_| Ok(Box::new(ScopedGovernor::new())))
            .unwrap_err();
        assert!(matches!(
            err,
            ComponentError::Conflict {
                family: "governor",
                ..
            }
        ));
    }

    #[test]
    fn a_factory_may_refuse_to_build_on_bad_settings() {
        let mut reg = ComponentRegistry::new();
        register_stdlib(&mut reg);
        // provider.rules requires `when_event_topic`; omit it.
        let result = reg.build_provider(&ComponentConfig::bare("provider.rules"));
        assert!(matches!(result, Err(ComponentError::Construction { .. })));
    }

    #[tokio::test]
    async fn built_components_wire_into_a_real_pipeline() {
        use crate::events::{EventStream, MemorySink};
        use crate::pipeline::{EffectRequest, Pipeline, PipelineError};
        use crate::registry::CapabilityRegistry;
        use crate::schema::{Capability, Scope};

        // Build a governor from the registry and use it to drive the pipeline —
        // proving the boxed component satisfies the same contract the core wants.
        let mut reg = ComponentRegistry::new();
        reg.register_governor("gov.scoped", |_| {
            Ok(Box::new(
                ScopedGovernor::new().grant("persona.assistant", ["cap.fs"]),
            ))
        })
        .unwrap();
        let governor = reg
            .build_governor(&ComponentConfig::bare("gov.scoped"))
            .unwrap();

        let mut caps = CapabilityRegistry::new();
        caps.register(Capability {
            id: "cap.fs.read".into(),
            summary: String::new(),
            args_schema: serde_json::json!({ "type": "object" }),
        })
        .unwrap();
        let mut stream = EventStream::spawn(MemorySink::new());
        let pipe = Pipeline {
            registry: &caps,
            governor: governor.as_ref(),
            executor: &crate::pipeline::EchoExecutor,
            events: &stream,
        };
        let denied = pipe
            .dispatch(EffectRequest {
                capability: "cap.fs.read".into(),
                args: serde_json::json!({}),
                correlation: None,
                scope: Scope::new("skill.rogue"),
            })
            .await;
        assert!(matches!(denied, Err(PipelineError::Rejected(_))));
        stream.shutdown();
    }
}
