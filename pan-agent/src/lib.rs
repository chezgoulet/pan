//! # pan-agent — `Agent.toml` and the assembler.
//!
//! The config model the plan settles before plugins proliferate (Design Decision
//! #1): one `Agent.toml` per agent instance is the source of truth for which
//! components an agent runs and what authority they carry. This crate parses it
//! ([`AgentManifest`]) and assembles it ([`assemble`]) into a scoped, wired
//! [`AssembledAgent`] — the point where the ADR-0001 interfaces (Scope,
//! ComponentRegistry) become a running graph instead of hand-wired code.
//!
//! ```
//! use pan_agent::{assemble_toml, builtin_registry};
//!
//! let agent = assemble_toml(
//!     r#"
//!     [meta]
//!     name = "demo"
//!     persona = "assistant"
//!     [persona]
//!     provider = "provider.behaviortree"
//!     [caps.grant]
//!     http = true
//!     "#,
//!     &builtin_registry(),
//! )
//! .unwrap();
//!
//! assert_eq!(agent.scope.origin, "persona.assistant");
//! ```

pub mod agent;
pub mod assembler;
pub mod builtin;
pub mod command;
pub mod echo;
pub mod manifest;
pub mod merge;

pub use assembler::{assemble, assemble_toml, assemble_with_config, AssembleError, AssembledAgent};
pub use builtin::builtin_registry;
pub use command::CommandProvider;
pub use echo::EchoProvider;
pub use manifest::{AgentManifest, Caps, ManifestError, Meta, Persona};
