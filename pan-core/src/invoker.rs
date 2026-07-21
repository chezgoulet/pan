//! # The `ScopedInvoker` — the one governed surface a skill holds (ADR 0001).
//!
//! A Python skill (or any sub-agent) that wants to touch the world does it by
//! **asking the pipeline to invoke a capability**, never by acting directly. The
//! object it holds to do that is a [`ScopedInvoker`]: a handle whose *only*
//! method is [`invoke`](ScopedInvoker::invoke), and every call routes through the
//! full dispatch pipeline (`resolve → validate → govern → execute`) under a
//! **bound [`Scope`]**.
//!
//! This is the invocation analogue of the read-only [`MemoryQuery`] grant in
//! [`crate::handles`]: the pattern is identical, and so is the guarantee. Just as
//! a `MemoryQuery` holder has no method to write and no way to recover the
//! writer, a `ScopedInvoker` holder:
//!
//! - **cannot reach the executor** — there is no method that performs an effect;
//!   only `invoke`, which goes through `govern` first (the [`Governed`] token
//!   still has no public constructor, so the dangerous path still doesn't
//!   compile);
//! - **cannot widen its own authority** — `invoke` takes only `(capability,
//!   args)`. The scope is bound *inside* the handle at mint time; there is no
//!   parameter through which a caller could name a different, broader origin.
//!
//! [`MemoryQuery`]: crate::handles::MemoryQuery
//! [`Governed`]: crate::pipeline::Governed
//!
//! ## Where the subprocess bridge fits
//!
//! For an out-of-process Python skill, a transport (JSON-lines over
//! stdin/stdout) turns each subprocess "invoke" message into one
//! `ScopedInvoker::invoke` call and streams the result back. The subprocess holds
//! **zero ambient authority** — no filesystem, no network — because the invoke
//! protocol is its only channel to the world. That transport is a thin adapter
//! over this trait; the governance guarantee lives here, in Rust, not in the
//! subprocess. The in-process [`PipelineInvoker`] below is the same handle a
//! Rust-side sub-agent would hold, and the tests exercise the full govern path
//! that the bridged case reuses unchanged.

use crate::pipeline::{EffectRequest, Pipeline, PipelineError, Verdict};
use crate::schema::{Scope, Value};

/// The single governed surface a skill / sub-agent holds to reach the world.
/// Every call is dispatched under the handle's bound scope; the holder cannot
/// name a different origin or skip a pipeline stage.
#[async_trait::async_trait]
pub trait ScopedInvoker: Send + Sync {
    /// Invoke a capability. Routes through the full pipeline under the bound
    /// scope; returns the executor's result, or a skill-facing [`InvokeError`]
    /// describing which stage refused.
    async fn invoke(&self, capability: &str, args: &Value) -> Result<Value, InvokeError>;

    /// The origin this invoker acts as. Diagnostic; carries no authority by
    /// itself (the governor's grants decide what the origin may reach).
    fn origin(&self) -> &str;
}

/// Why an [`invoke`](ScopedInvoker::invoke) did not return a result. A
/// deliberately small, skill-facing projection of [`PipelineError`] — a skill
/// learns *that* it was denied or that its args were bad, never the governor's
/// internals or the executor's guts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvokeError {
    /// The capability id is not registered (the origin asked for a verb that
    /// does not exist in this deployment).
    NotFound { capability: String },
    /// The args did not match the capability's schema.
    InvalidArgs { capability: String, reason: String },
    /// Governance refused this invocation for this origin.
    Denied { capability: String, reason: String },
    /// The capability ran but failed.
    Failed { capability: String, reason: String },
}

impl std::fmt::Display for InvokeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvokeError::NotFound { capability } => {
                write!(f, "no such capability `{capability}`")
            }
            InvokeError::InvalidArgs { capability, reason } => {
                write!(f, "invalid args for `{capability}`: {reason}")
            }
            InvokeError::Denied { capability, reason } => {
                write!(f, "denied `{capability}`: {reason}")
            }
            InvokeError::Failed { capability, reason } => {
                write!(f, "`{capability}` failed: {reason}")
            }
        }
    }
}

impl std::error::Error for InvokeError {}

impl From<PipelineError> for InvokeError {
    fn from(e: PipelineError) -> Self {
        match e {
            PipelineError::Unresolved { capability } => InvokeError::NotFound { capability },
            PipelineError::Invalid { capability, reason } => {
                InvokeError::InvalidArgs { capability, reason }
            }
            PipelineError::Rejected(r) => {
                let reason = match r.verdict {
                    Verdict::Deny { reason } | Verdict::RequireApproval { reason } => reason,
                    // `govern` only produces a Rejected on a non-Allow verdict.
                    Verdict::Allow => "allowed".to_string(),
                };
                InvokeError::Denied {
                    capability: r.capability,
                    reason,
                }
            }
            PipelineError::Execution { capability, reason } => {
                InvokeError::Failed { capability, reason }
            }
        }
    }
}

/// The in-process [`ScopedInvoker`]: dispatches through a borrowed [`Pipeline`]
/// under a fixed scope. This is exactly the handle a Rust-side sub-agent holds,
/// and the object the subprocess bridge drives on behalf of a Python skill.
pub struct PipelineInvoker<'a> {
    pipeline: &'a Pipeline<'a>,
    scope: Scope,
}

impl<'a> PipelineInvoker<'a> {
    /// Mint an invoker that acts under `scope` against `pipeline`.
    pub fn new(pipeline: &'a Pipeline<'a>, scope: Scope) -> Self {
        Self { pipeline, scope }
    }

    /// Mint a **sub-invoker** for a nested skill: the same pipeline, a different
    /// origin. This does not grant anything — it only stamps a new origin string.
    pub fn sub(&self, origin: impl Into<String>) -> PipelineInvoker<'a> {
        PipelineInvoker {
            pipeline: self.pipeline,
            scope: Scope::new(origin),
        }
    }
}

#[async_trait::async_trait]
impl<'a> ScopedInvoker for PipelineInvoker<'a> {
    async fn invoke(&self, capability: &str, args: &Value) -> Result<Value, InvokeError> {
        let req = EffectRequest {
            capability: capability.to_string(),
            args: args.clone(),
            correlation: None,
            scope: self.scope.clone(),
        };
        self.pipeline
            .dispatch(req)
            .await
            .map(|effected| effected.result)
            .map_err(InvokeError::from)
    }

    fn origin(&self) -> &str {
        &self.scope.origin
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventStream, MemorySink};
    use crate::pipeline::{EchoExecutor, ScopedGovernor};
    use crate::registry::CapabilityRegistry;
    use crate::schema::Capability;

    fn registry_with(ids: &[&str]) -> CapabilityRegistry {
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

    /// A stand-in for a skill: it holds ONLY a `&dyn ScopedInvoker` — no pipeline,
    /// no registry, no executor — and can therefore touch the world *only* by
    /// asking. This mirrors what a bridged Python subprocess can express.
    async fn skill_reads_then_writes(inv: &dyn ScopedInvoker) -> Vec<Result<Value, InvokeError>> {
        vec![
            inv.invoke("cap.fs.read", &serde_json::json!({ "path": "notes.md" }))
                .await,
            inv.invoke("cap.fs.write", &serde_json::json!({ "path": "out.md" }))
                .await,
        ]
    }

    #[tokio::test]
    async fn skill_invokes_are_governed_by_its_bound_scope() {
        // The skill's origin may reach cap.fs.* but not cap.shell.*.
        let reg = registry_with(&["cap.fs.read", "cap.fs.write", "cap.shell.run"]);
        let gov = ScopedGovernor::new().grant("skill.notetaker", ["cap.fs"]);
        let mut stream = EventStream::spawn(MemorySink::new());
        let pipe = Pipeline {
            registry: &reg,
            governor: &gov,
            executor: &EchoExecutor,
            events: &stream,
        };
        let inv = PipelineInvoker::new(&pipe, Scope::new("skill.notetaker"));

        let results = skill_reads_then_writes(&inv).await;
        assert!(results[0].is_ok(), "cap.fs.read is within grant");
        assert!(results[1].is_ok(), "cap.fs.write is within grant");

        // The same skill reaching outside its grant is denied at govern — it
        // cannot escape its scope even though the capability is registered.
        let escalation = inv
            .invoke("cap.shell.run", &serde_json::json!({ "cmd": "rm -rf /" }))
            .await;
        assert!(
            matches!(escalation, Err(InvokeError::Denied { .. })),
            "out-of-scope invoke must be denied, got {escalation:?}"
        );
        stream.shutdown();
    }

    #[tokio::test]
    async fn a_sub_invoker_cannot_widen_authority() {
        // A skill mints a sub-invoker naming a more-privileged origin. Naming it
        // grants nothing: the governor has no entry for "persona.admin" here, so
        // the sub-invoker is denied. Authority lives in the grant table, not in
        // the string a caller picks.
        let reg = registry_with(&["cap.shell.run"]);
        let gov = ScopedGovernor::new().grant("skill.notetaker", ["cap.fs"]);
        let mut stream = EventStream::spawn(MemorySink::new());
        let pipe = Pipeline {
            registry: &reg,
            governor: &gov,
            executor: &EchoExecutor,
            events: &stream,
        };
        let inv = PipelineInvoker::new(&pipe, Scope::new("skill.notetaker"));

        let forged = inv.sub("persona.admin");
        let attempt = forged
            .invoke("cap.shell.run", &serde_json::json!({ "cmd": "id" }))
            .await;
        assert!(
            matches!(attempt, Err(InvokeError::Denied { .. })),
            "an unauthorized origin string grants nothing, got {attempt:?}"
        );
        stream.shutdown();
    }

    #[tokio::test]
    async fn unknown_capability_surfaces_as_not_found() {
        let reg = registry_with(&[]);
        let gov = ScopedGovernor::new().grant("skill.x", ["cap"]);
        let mut stream = EventStream::spawn(MemorySink::new());
        let pipe = Pipeline {
            registry: &reg,
            governor: &gov,
            executor: &EchoExecutor,
            events: &stream,
        };
        let inv = PipelineInvoker::new(&pipe, Scope::new("skill.x"));
        let r = inv.invoke("cap.nope", &serde_json::json!({})).await;
        assert!(matches!(r, Err(InvokeError::NotFound { .. })));
        stream.shutdown();
    }
}
