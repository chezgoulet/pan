//! # The assembler — `Agent.toml` → a scoped, wired agent.
//!
//! [`assemble`] turns a parsed [`AgentManifest`] into an [`AssembledAgent`]: the
//! persona's [`Scope`], a [`ScopedGovernor`] built from `[caps.grant]`, and the
//! provider built through a [`ComponentRegistry`]. This is where the config model
//! (D1 Scope, D3 ComponentRegistry) becomes a running graph — no hand-wiring, no
//! hard-coded provider. Naming a provider the binary wasn't built with, or a
//! provider whose settings are bad, is a load-time error, not a late surprise.

use pan_core::components::{ComponentConfig, ComponentError, ComponentRegistry};
use pan_core::pipeline::ScopedGovernor;
use pan_core::schema::{Provider, Scope, Value};

use crate::manifest::{AgentManifest, ManifestError};

/// A fully-wired agent: everything a loop/pipeline needs, built from config.
pub struct AssembledAgent {
    /// The agent instance name (`meta.name`).
    pub name: String,
    /// The system prompt / role for this persona.
    pub instruction: String,
    /// The authority every effect this agent dispatches is stamped with.
    pub scope: Scope,
    /// Governance built from `[caps.grant]`: the persona's origin is granted its
    /// declared capability prefixes, everything else denied.
    pub governor: ScopedGovernor,
    /// The provider component named by `persona.provider`.
    pub provider: Box<dyn Provider>,
}

/// Assemble an agent from its manifest, building components out of `registry`.
pub fn assemble(
    manifest: &AgentManifest,
    registry: &ComponentRegistry,
) -> Result<AssembledAgent, AssembleError> {
    let origin = manifest.origin();

    // Governance: the persona's origin is granted exactly the prefixes it declared.
    let governor = ScopedGovernor::new().grant(origin.clone(), manifest.granted_prefixes());

    // Provider: built via the registry from the persona's id + settings. The
    // settings carry the instruction and (optional) model, plus anything else a
    // provider factory looks for.
    let mut settings = serde_json::Map::new();
    settings.insert(
        "instruction".into(),
        Value::String(manifest.persona.instruction.clone()),
    );
    if let Some(model) = &manifest.persona.model {
        settings.insert("model".into(), Value::String(model.clone()));
    }
    let cfg = ComponentConfig::new(manifest.persona.provider.clone(), Value::Object(settings));
    let provider = registry
        .build_provider(&cfg)
        .map_err(AssembleError::Provider)?;

    Ok(AssembledAgent {
        name: manifest.meta.name.clone(),
        instruction: manifest.persona.instruction.clone(),
        scope: Scope::new(origin),
        governor,
        provider,
    })
}

/// Parse an `Agent.toml` and assemble it in one step.
pub fn assemble_toml(
    text: &str,
    registry: &ComponentRegistry,
) -> Result<AssembledAgent, AssembleError> {
    let manifest = AgentManifest::from_toml(text).map_err(AssembleError::Manifest)?;
    assemble(&manifest, registry)
}

/// Why an agent could not be assembled.
#[derive(Debug)]
pub enum AssembleError {
    /// The manifest itself was bad (parse / validation).
    Manifest(ManifestError),
    /// A named component could not be built (unknown id, or bad settings).
    Provider(ComponentError),
}

impl std::fmt::Display for AssembleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssembleError::Manifest(e) => write!(f, "{e}"),
            AssembleError::Provider(e) => write!(f, "assembling provider: {e}"),
        }
    }
}

impl std::error::Error for AssembleError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::builtin_registry;
    use pan_core::events::{EventStream, MemorySink};
    use pan_core::pipeline::{EchoExecutor, EffectRequest, Pipeline, PipelineError};
    use pan_core::registry::CapabilityRegistry;
    use pan_core::schema::Capability;

    const SAMPLE: &str = r#"
[meta]
name = "pan-default"
persona = "assistant"

[persona]
instruction = "You are a helpful agent."
provider = "provider.behaviortree"

[caps.grant]
shell = true
fs = false
"#;

    fn caps(ids: &[&str]) -> CapabilityRegistry {
        let mut r = CapabilityRegistry::new();
        for id in ids {
            r.register(Capability {
                id: (*id).into(),
                summary: String::new(),
                args_schema: serde_json::json!({ "type": "object" }),
            })
            .unwrap();
        }
        r
    }

    #[test]
    fn assembles_scope_governor_and_provider_from_config() {
        let agent = assemble_toml(SAMPLE, &builtin_registry()).unwrap();
        assert_eq!(agent.name, "pan-default");
        assert_eq!(agent.scope.origin, "persona.assistant");
        assert_eq!(agent.instruction, "You are a helpful agent.");
        assert_eq!(agent.provider.id(), "provider.behaviortree");
    }

    #[test]
    fn unknown_provider_is_a_load_error() {
        let bad = r#"
[meta]
name = "x"
[persona]
provider = "provider.does_not_exist"
"#;
        // `AssembledAgent` isn't Debug (it holds a `dyn Provider`), so match the
        // Result rather than unwrap_err.
        let result = assemble_toml(bad, &builtin_registry());
        assert!(matches!(
            result,
            Err(AssembleError::Provider(ComponentError::Unknown { .. }))
        ));
    }

    /// The end-to-end payoff: the governor the assembler built from `[caps.grant]`
    /// actually gates a dispatch — `shell` is granted, `fs` is not, under the
    /// persona's origin. Config → enforcement, no hand-wiring.
    #[tokio::test]
    async fn assembled_governor_enforces_the_config_grants() {
        let agent = assemble_toml(SAMPLE, &builtin_registry()).unwrap();
        let reg = caps(&["cap.shell.run", "cap.fs.read"]);
        let mut stream = EventStream::spawn(MemorySink::new());
        let pipe = Pipeline {
            registry: &reg,
            governor: &agent.governor,
            executor: &EchoExecutor,
            events: &stream,
        };

        let shell = pipe
            .dispatch(EffectRequest {
                capability: "cap.shell.run".into(),
                args: serde_json::json!({}),
                correlation: None,
                scope: agent.scope.clone(),
            })
            .await;
        assert!(shell.is_ok(), "shell=true should be granted by config");

        let fs = pipe
            .dispatch(EffectRequest {
                capability: "cap.fs.read".into(),
                args: serde_json::json!({}),
                correlation: None,
                scope: agent.scope.clone(),
            })
            .await;
        assert!(
            matches!(fs, Err(PipelineError::Rejected(_))),
            "fs=false must be denied by config, got {fs:?}"
        );
        stream.shutdown();
    }
}
