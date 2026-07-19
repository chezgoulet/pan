//! # `Agent.toml` — the agent manifest.
//!
//! One file per agent instance. It is the single source of truth for *which*
//! components an agent runs and *what authority* they carry — the config model
//! the plan settles before plugins proliferate (Design Decision #1: Agent.toml,
//! not env vars). Environment variables may override individual fields later, but
//! never define the whole graph.
//!
//! ```toml
//! [meta]
//! name = "pan-default"
//! persona = "assistant"
//!
//! [persona]
//! instruction = "You are a helpful agent running in a terminal."
//! provider = "provider.rules"      # a ComponentRegistry id
//! model = "claude-sonnet-4-6"      # optional, provider-specific
//!
//! [caps.grant]
//! shell = true
//! fs = false
//! http = true
//! memory = true
//! ```
//!
//! A **persona** binds one concept: the capabilities the agent may invoke (its
//! `[caps.grant]`, which becomes a [`Scope`](pan_core::schema::Scope) + a scoped
//! governor), the instructions it follows, and which provider drives it. The
//! assembler ([`crate::assemble`]) turns this into a runnable, governed unit.

use std::collections::BTreeMap;

use serde::Deserialize;

/// A parsed `Agent.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentManifest {
    pub meta: Meta,
    pub persona: Persona,
    #[serde(default)]
    pub caps: Caps,
}

/// `[meta]` — the agent instance's identity.
#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    /// The agent instance name (for logs, audit, and the default persona label).
    pub name: String,
    /// The persona label; also the governance origin (`persona.<label>`). Falls
    /// back to `name` when omitted.
    #[serde(default)]
    pub persona: Option<String>,
}

/// `[persona]` — what the agent is: its voice, its brain, its capabilities
/// (the last via [`Caps`]). One concept, declared in one place.
#[derive(Debug, Clone, Deserialize)]
pub struct Persona {
    /// The system prompt / role / voice. Consumed by whichever provider wants it.
    #[serde(default)]
    pub instruction: String,
    /// The provider component id (a [`ComponentRegistry`](pan_core::components::ComponentRegistry)
    /// key), e.g. `"provider.rules"`, `"provider.anthropic"`.
    pub provider: String,
    /// Optional provider-specific model id, passed to the provider factory.
    #[serde(default)]
    pub model: Option<String>,
}

/// `[caps]` — the persona's authority. `grant` is a friendly per-family switch;
/// each `<family> = true` grants the capability-id prefix `cap.<family>` (so
/// `shell = true` admits `cap.shell`, `cap.shell.run`, …). Deny-by-default: a
/// family that is `false` or absent is not granted.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Caps {
    #[serde(default)]
    pub grant: BTreeMap<String, bool>,
}

impl AgentManifest {
    /// Parse and validate a manifest from TOML text.
    pub fn from_toml(text: &str) -> Result<Self, ManifestError> {
        let manifest: AgentManifest =
            toml::from_str(text).map_err(|e| ManifestError::Parse(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Load a manifest from a file path.
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, ManifestError> {
        let text =
            std::fs::read_to_string(path.as_ref()).map_err(|e| ManifestError::Io(e.to_string()))?;
        Self::from_toml(&text)
    }

    /// The persona label — `meta.persona`, or `meta.name` when unset.
    pub fn persona_label(&self) -> &str {
        self.meta.persona.as_deref().unwrap_or(&self.meta.name)
    }

    /// The governance origin this persona acts under: `persona.<label>`. This is
    /// what the governor keys grants off, so an agent cannot act as an origin its
    /// manifest did not declare.
    pub fn origin(&self) -> String {
        format!("persona.{}", self.persona_label())
    }

    /// The granted capability-id prefixes (families set to `true`, mapped to
    /// `cap.<family>`), sorted and de-duplicated.
    pub fn granted_prefixes(&self) -> Vec<String> {
        self.caps
            .grant
            .iter()
            .filter(|&(_, &on)| on)
            .map(|(family, _)| format!("cap.{family}"))
            .collect()
    }

    fn validate(&self) -> Result<(), ManifestError> {
        if self.meta.name.trim().is_empty() {
            return Err(ManifestError::Invalid("meta.name must not be empty".into()));
        }
        if self.persona.provider.trim().is_empty() {
            return Err(ManifestError::Invalid(
                "persona.provider must name a component".into(),
            ));
        }
        Ok(())
    }
}

/// Why a manifest could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Io(String),
    Parse(String),
    Invalid(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Io(e) => write!(f, "reading Agent.toml: {e}"),
            ManifestError::Parse(e) => write!(f, "parsing Agent.toml: {e}"),
            ManifestError::Invalid(e) => write!(f, "invalid Agent.toml: {e}"),
        }
    }
}

impl std::error::Error for ManifestError {}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[meta]
name = "pan-default"
persona = "assistant"

[persona]
instruction = "You are a helpful agent."
provider = "provider.rules"

[caps.grant]
shell = true
fs = false
http = true
memory = true
"#;

    #[test]
    fn parses_and_derives_origin_and_grants() {
        let m = AgentManifest::from_toml(SAMPLE).unwrap();
        assert_eq!(m.meta.name, "pan-default");
        assert_eq!(m.persona_label(), "assistant");
        assert_eq!(m.origin(), "persona.assistant");
        assert_eq!(m.persona.provider, "provider.rules");

        // shell + http + memory are granted; fs is not.
        let prefixes = m.granted_prefixes();
        assert!(prefixes.contains(&"cap.shell".to_string()));
        assert!(prefixes.contains(&"cap.http".to_string()));
        assert!(prefixes.contains(&"cap.memory".to_string()));
        assert!(!prefixes.contains(&"cap.fs".to_string()));
    }

    #[test]
    fn persona_label_falls_back_to_name() {
        let m = AgentManifest::from_toml(
            r#"
[meta]
name = "solo"
[persona]
provider = "provider.behaviortree"
"#,
        )
        .unwrap();
        assert_eq!(m.persona_label(), "solo");
        assert_eq!(m.origin(), "persona.solo");
        assert!(
            m.granted_prefixes().is_empty(),
            "no grants = deny by default"
        );
    }

    #[test]
    fn malformed_toml_is_an_error() {
        assert!(matches!(
            AgentManifest::from_toml("this is not toml = = ="),
            Err(ManifestError::Parse(_))
        ));
    }

    #[test]
    fn empty_provider_is_rejected() {
        let err = AgentManifest::from_toml(
            r#"
[meta]
name = "x"
[persona]
provider = ""
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::Invalid(_)));
    }
}
