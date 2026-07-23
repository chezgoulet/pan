//! # The assembler — `Agent.toml` → a scoped, wired agent.
//!
//! [`assemble`] turns a parsed [`AgentManifest`] into an [`AssembledAgent`]: the
//! persona's [`Scope`], a [`ScopedGovernor`] built from `[caps.grant]`, and the
//! provider built through a [`ComponentRegistry`]. This is where the config model
//! (D1 Scope, D3 ComponentRegistry) becomes a running graph — no hand-wiring, no
//! hard-coded provider. Naming a provider the binary wasn't built with, or a
//! provider whose settings are bad, is a load-time error, not a late surprise.

use pan_core::components::{ComponentConfig, ComponentError, ComponentRegistry};
use pan_core::config::Config;
use pan_core::pipeline::ScopedGovernor;
use pan_core::registry::ConflictError;
use pan_core::schema::{ContextAssembler, Provider, Scope, Value};
use pan_core::toolbox::Toolbox;

use crate::manifest::{AgentManifest, ManifestError};

/// A fully-wired agent: everything a loop/pipeline needs, built from config.
///
/// The four pieces compose directly: `toolbox.registry()` is the pipeline's
/// capability registry, `&toolbox` is its executor, `governor` is its govern
/// stage, and `provider` + `scope` drive the loop.
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
    /// The capability components from `[caps.enable]`: the pipeline's capability
    /// registry (via [`Toolbox::registry`]) and its executor (`&toolbox`).
    pub toolbox: Toolbox,
    /// Optional context assembler for building/governing the span context.
    pub context_assembler: Option<Box<dyn ContextAssembler>>,
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
    // settings carry the instruction and (optional) model, plus any other
    // `[persona]` keys a provider factory looks for (e.g. a rules array).
    let mut settings = serde_json::Map::new();
    settings.insert(
        "instruction".into(),
        Value::String(manifest.persona.instruction.clone()),
    );
    if let Some(model) = &manifest.persona.model {
        settings.insert("model".into(), Value::String(model.clone()));
    }
    for (key, value) in &manifest.persona.settings {
        settings.insert(key.clone(), toml_to_json(value));
    }
    let cfg = ComponentConfig::new(manifest.persona.provider.clone(), Value::Object(settings));
    let provider = registry
        .build_provider(&cfg)
        .map_err(AssembleError::Provider)?;

    // Toolbox: the capability components the persona enables, each with its own
    // `[caps.settings."cap.x"]` config. This is what the agent can actually *do*.
    let mut toolbox = Toolbox::new();
    for id in &manifest.caps.enable {
        let cap_settings = manifest
            .caps
            .settings
            .get(id)
            .map(toml_to_json)
            .unwrap_or(Value::Null);
        let cfg = ComponentConfig::new(id.clone(), cap_settings);
        let component = registry
            .build_capability_provider(&cfg)
            .map_err(AssembleError::Capability)?;
        toolbox
            .add(component)
            .map_err(AssembleError::CapabilityConflict)?;
    }

    // Context assembler (optional): builds the span context from conversation
    // history or other sources. Selected by `[persona] context = "..."`.
    let context_assembler = manifest
        .persona
        .settings
        .get("context")
        .and_then(|v| v.as_str())
        .map(|id| {
            let cfg = ComponentConfig::bare(id.to_string());
            registry
                .build_context_assembler(&cfg)
                .map_err(AssembleError::ContextAssembler)
        })
        .transpose()?;

    Ok(AssembledAgent {
        name: manifest.meta.name.clone(),
        instruction: manifest.persona.instruction.clone(),
        scope: Scope::new(origin),
        governor,
        provider,
        toolbox,
        context_assembler,
    })
}

/// Assemble an agent with global config merging. Global settings from
/// `~/.pan/config.toml` serve as defaults that per-agent settings override.
/// Pass `None` for `global` when no global config is available (same
/// behavior as [`assemble`]).
pub fn assemble_with_config(
    manifest: &AgentManifest,
    registry: &ComponentRegistry,
    global: Option<&Config>,
) -> Result<AssembledAgent, AssembleError> {
    let origin = manifest.origin();
    let governor = ScopedGovernor::new().grant(origin.clone(), manifest.granted_prefixes());

    // Provider with merged global + per-agent settings.
    let merged_provider = crate::merge::merge_provider_settings(global, manifest);
    let cfg = ComponentConfig::new(manifest.persona.provider.clone(), merged_provider);
    let provider = registry
        .build_provider(&cfg)
        .map_err(AssembleError::Provider)?;

    // Toolbox with merged global + per-agent settings per capability.
    let mut toolbox = Toolbox::new();
    for id in &manifest.caps.enable {
        let merged_cap = crate::merge::merge_cap_settings(global, manifest, id);
        let cfg = ComponentConfig::new(id.clone(), merged_cap);
        let component = registry
            .build_capability_provider(&cfg)
            .map_err(AssembleError::Capability)?;
        toolbox
            .add(component)
            .map_err(AssembleError::CapabilityConflict)?;
    }

    // Context assembler (optional).
    let context_assembler = manifest
        .persona
        .settings
        .get("context")
        .and_then(|v| v.as_str())
        .map(|id| {
            let cfg = ComponentConfig::bare(id.to_string());
            registry
                .build_context_assembler(&cfg)
                .map_err(AssembleError::ContextAssembler)
        })
        .transpose()?;

    Ok(AssembledAgent {
        name: manifest.meta.name.clone(),
        instruction: manifest.persona.instruction.clone(),
        scope: Scope::new(origin),
        governor,
        provider,
        toolbox,
        context_assembler,
    })
}

/// Convert a TOML value (from the manifest) into the core `Value` the component
/// factories consume. TOML and JSON share the same data model here, so this is a
/// faithful re-serialization.
fn toml_to_json(value: &toml::Value) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
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
    /// The provider component could not be built (unknown id, or bad settings).
    Provider(ComponentError),
    /// An enabled capability component could not be built (unknown id, or bad
    /// settings — e.g. `cap.fs` without a `root`).
    Capability(ComponentError),
    /// Two enabled capability components claimed the same capability id.
    CapabilityConflict(ConflictError),
    /// The context assembler could not be built (unknown id).
    ContextAssembler(ComponentError),
}

impl std::fmt::Display for AssembleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssembleError::Manifest(e) => write!(f, "{e}"),
            AssembleError::Provider(e) => write!(f, "assembling provider: {e}"),
            AssembleError::Capability(e) => write!(f, "assembling capability: {e}"),
            AssembleError::CapabilityConflict(e) => write!(f, "capability conflict: {e}"),
            AssembleError::ContextAssembler(e) => write!(f, "context assembler: {e}"),
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
            hooks: vec![],
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
