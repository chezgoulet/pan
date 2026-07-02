//! # `ResolveGovernor` — the daemon's `govern` stage.
//!
//! The Soul Protocol's wire contract: an `ActionIntent::Invoke` of a capability
//! the host hasn't registered is a **bug** — the host's `register_capabilities`
//! message defined the universe of valid verbs. The wire explicitly closes this
//! loop with `error code: "unknown_capability"` (fixture 09), and the resolve
//! stage in pan-core's pipeline already returns `PipelineError::Unresolved`.
//!
//! This module wraps that pipeline-stage error in a *type* the session can use
//! to build the wire-level `error` reply, and it confirms the daemon's only
//! governance policy at M1 is **allow what the host registered; reject the
//! rest**. That is `ResolveGovernor::govern(capability, args) == Allow iff
//! registry.lookup(capability).is_some()`. Arg-shape validation remains the
//! pipeline's `validate` stage; this stage only asks "is this verb admitted
//! by the host?"
//!
//! ## Why the daemon's first governor IS the resolver
//!
//! pan-core's pipeline does `resolve -> validate -> govern -> execute`.
//! `resolve` looks up the capability in the registry. If the capability is
//! missing, the pipeline never reaches `govern` — the dispatch returns
//! `PipelineError::Unresolved`. The session code catches that and emits the
//! `error: unknown_capability` wire message. So at M1, the "governor" the
//! session consults is structurally just the registry check; later waves can
//! insert a `gov.policy` between `validate` and `execute` without changing the
//! wire.

use pan_core::pipeline::{Governor, Verdict};
use pan_core::registry::CapabilityRegistry;
use pan_core::schema::Value;

/// The daemon's M1 governor. Allow iff the host registered this capability.
/// Holds a read-only reference to the per-soul capability registry.
pub struct ResolveGovernor<'a> {
    pub registry: &'a CapabilityRegistry,
}

impl<'a> Governor for ResolveGovernor<'a> {
    fn id(&self) -> &str { "gov.daemon.resolve" }

    fn govern(&self, capability: &str, _args: &Value) -> Verdict {
        if self.registry.lookup(capability).is_some() {
            Verdict::Allow
        } else {
            // The reason field is informational (logged, not returned on the
            // wire); the wire-level `unknown_capability` is produced by the
            // session when it catches `PipelineError::Unresolved` BEFORE we get
            // here. This branch exists for symmetry and so a future caller can
            // route a synthetic Invoke through govern without first resolving.
            Verdict::Deny { reason: format!("capability `{capability}` is not registered") }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pan_core::schema::Capability;

    fn reg_with(caps: &[&str]) -> CapabilityRegistry {
        let mut r = CapabilityRegistry::new();
        for id in caps {
            r.register(Capability {
                id: (*id).into(),
                summary: String::new(),
                args_schema: serde_json::json!({"type": "object"}),
            }).unwrap();
        }
        r
    }

    #[test]
    fn registered_capability_is_allowed() {
        let r = reg_with(&["npc.move_to"]);
        let g = ResolveGovernor { registry: &r };
        assert!(matches!(g.govern("npc.move_to", &Value::Null), Verdict::Allow));
    }

    #[test]
    fn unregistered_capability_is_denied_with_explicit_reason() {
        let r = reg_with(&["npc.move_to"]);
        let g = ResolveGovernor { registry: &r };
        let v = g.govern("npc.fly_ship", &Value::Null);
        match v {
            Verdict::Deny { reason } => {
                assert!(reason.contains("npc.fly_ship"),
                    "reason should name the unknown capability: {reason}");
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }
}
