//! # `cap.agent` — multi-agent delegation capability.
//!
//! Provides `cap.agent.delegate`: a governed capability that loads a child
//! agent from its `Agent.toml`, assembles it with a delegation-narrowed
//! scope, runs it against a goal, and returns the result. An LLM invokes
//! this through the ReAct loop to spawn sub-agents for sub-tasks.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pan_core::components::{ComponentConfig, ComponentError, ComponentRegistry};
use pan_core::events::{DiscardSink, EventStream};
use pan_core::invoker::ScopedInvoker;
use pan_core::loop_engine::{Loop, Once, NO_VETO};
use pan_core::pipeline::{ExecError, Pipeline};
use pan_core::schema::{Capability, Context, Goal, Scope, Trigger, Value};
use pan_core::toolbox::CapabilityProvider;

use crate::builtin::builtin_registry;
use crate::manifest::AgentManifest;
use crate::AssembledAgent;

const MAX_DELEGATION_DEPTH: u32 = 3;

pub struct AgentCaps {
    agents_dir: PathBuf,
    registry: Arc<ComponentRegistry>,
}

impl AgentCaps {
    pub fn new(agents_dir: impl Into<PathBuf>) -> Self {
        Self {
            agents_dir: agents_dir.into(),
            registry: Arc::new(builtin_registry()),
        }
    }

    fn load_child(&self, name: &str) -> Result<AssembledAgent, String> {
        let path = self.agents_dir.join(format!("{name}.toml"));
        let manifest = AgentManifest::load(&path).map_err(|e| format!("load {name}: {e}"))?;
        crate::assemble(&manifest, &self.registry).map_err(|e| format!("assemble {name}: {e}"))
    }
}

#[async_trait::async_trait]
impl CapabilityProvider for AgentCaps {
    fn id(&self) -> &str {
        "cap.agent"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability {
            id: "cap.agent.delegate".into(),
            summary: "Delegate a goal to a child agent. The child is loaded from \
                       its Agent.toml and runs under a narrowed scope."
                .into(),
            args_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": {"type": "string", "description": "Child agent name (maps to {agent}.toml)"},
                    "objective": {"type": "string", "description": "The goal objective for the child"},
                    "input": {"type": "object", "description": "Structured input to pass as context"}
                },
                "required": ["agent", "objective"]
            }),
        }]
    }

    async fn execute(&self, _capability: &str, _args: &Value) -> Result<Value, ExecError> {
        Err(ExecError(
            "cap.agent.delegate requires a ScopedInvoker (cannot be invoked directly)".into(),
        ))
    }

    async fn execute_with_invoker(
        &self,
        _capability: &str,
        args: &Value,
        invoker: &dyn ScopedInvoker,
    ) -> Result<Value, ExecError> {
        let agent_name = args
            .get("agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExecError("`agent` must be a string".into()))?;
        let objective = args
            .get("objective")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExecError("`objective` must be a string".into()))?;

        // Check delegation depth: count dots in the origin to avoid loops.
        let depth = invoker.origin().matches(".delegated.").count() as u32 + 1;
        if depth > MAX_DELEGATION_DEPTH {
            return Err(ExecError(format!(
                "delegation depth {depth} exceeds max {MAX_DELEGATION_DEPTH}"
            )));
        }

        // Load and assemble the child agent.
        let child = self.load_child(agent_name).map_err(ExecError)?;

        // Narrow the scope: child acts under parent's delegation chain.
        let child_scope = Scope::new(format!("{}.delegated.{agent_name}", invoker.origin()));

        let goal = Goal {
            id: format!(
                "delegate-{}",
                std::time::UNIX_EPOCH
                    .elapsed()
                    .unwrap_or_default()
                    .as_nanos()
            ),
            revision: 0,
            objective: objective.to_string(),
            trigger: Trigger::Utterance {
                from: "delegate".into(),
                content: objective.to_string(),
            },
        };

        let registry = child.toolbox.registry();
        let mut stream = EventStream::spawn(DiscardSink);
        let pipeline = Pipeline {
            registry: &registry,
            governor: &child.governor,
            executor: &child.toolbox,
            events: &stream,
            hooks: vec![],
        };
        let lp = Loop {
            provider: child.provider.as_ref(),
            pipeline: &pipeline,
            events: &stream,
            scope: child_scope,
            token_tx: None,
            veto_source: NO_VETO,
            stall_detector: None,
            compactor: None,
            context_budget: None,
            evaluator: None,
        };
        let mut obs = Once(Some(goal));
        let report = lp.run_span(&mut obs, &Context::default()).await;
        stream.shutdown();

        Ok(serde_json::json!({
            "expressed": report.expressed,
            "results": report.results,
            "end": report.end.map(|e| format!("{e:?}")),
        }))
    }
}

/// Builder for use with ComponentRegistry.
fn build_agent_caps(cfg: &ComponentConfig) -> Result<Box<dyn CapabilityProvider>, ComponentError> {
    let agents_dir = cfg
        .settings
        .get("agents_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ComponentError::Construction {
            id: cfg.id.clone(),
            reason: "cap.agent requires `agents_dir` setting".into(),
        })?;
    Ok(Box::new(AgentCaps::new(Path::new(agents_dir))))
}

pub fn register_agent_cap(registry: &mut pan_core::components::ComponentRegistry) {
    registry
        .register_capability_provider("cap.agent", build_agent_caps)
        .expect("register cap.agent");
}
