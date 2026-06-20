//! # Pan core vocabulary — the `Goal` / `ActionIntent` contract.
//!
//! This is the make-or-break contract from the spec (§12, first open question):
//! the typed shape that an LLM provider AND a non-LLM provider can both emit
//! *natively*, without either one cosplaying the other.
//!
//! The honesty test for this contract lives in `crate::providers`: three
//! providers (`provider.llm`, `provider.behaviortree`, `provider.rules`) are
//! implemented against the SAME types. If any of them has to fabricate a field
//! that only makes sense for a different provider, the schema has leaked.
//!
//! v1.0 (reconciled to the complete report): `ActionIntent` has THREE variants —
//! `Invoke`, `Express`, `Conclude`. State writes are `Invoke` of a state-write
//! capability, not a separate `Mutate`. `Goal` carries a `revision` for the
//! streaming-supersession / abandon-path mechanism.

use serde::{Deserialize, Serialize};

/// Thin alias so the contract doesn't hard-depend on serde_json in its public
/// surface vocabulary (and so a future swap is a one-line change).
pub type Value = serde_json::Value;

// ---------------------------------------------------------------------------
// What the core hands TO a provider.
// ---------------------------------------------------------------------------

/// A `Goal` is the thing-to-be-pursued this step. It is deliberately NOT a
/// chat message: a chat turn, a cron tick, a game event, and a sensor threshold
/// all reduce to "here is what we're trying to achieve, and what just happened."
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Goal {
    /// Stable identity of the pursuit across steps (a run may take many steps).
    pub id: String,
    /// Monotonic version of this goal within its span. A new revision SUPERSEDES
    /// the prior; the loop decides on the latest, and a Decision whose goal was
    /// superseded is discarded at the `enact` boundary rather than executed. This
    /// is the streaming/voice-revision mechanism and shares its abandon-path with
    /// the (deferred) hardware safety veto.
    pub revision: u64,
    /// Human/agent-readable statement of intent. For an LLM this becomes part of
    /// the prompt; for a behavior tree it's a blackboard label; for rules it's
    /// metadata. No provider is REQUIRED to consume it.
    pub objective: String,
    /// The triggering occurrence, normalized. The provider decides what, if
    /// anything, to do about it.
    pub trigger: Trigger,
}

impl Goal {
    /// A goal `other` supersedes `self` iff it shares identity and carries a
    /// strictly newer revision. This is the single predicate the abandon-path
    /// keys off; see [`crate::loop_engine`].
    pub fn superseded_by(&self, other: &Goal) -> bool {
        self.id == other.id && other.revision > self.revision
    }
}

/// Why this step is happening. Generic over deployment: every `Input` source in
/// the spec collapses into one of these.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    /// Someone said something. `content` is free text; `from` is an opaque id.
    Utterance { from: String, content: String },
    /// A scheduled or recurring tick fired.
    Tick { sequence: u64 },
    /// A discrete event arrived (game event, webhook, etc.).
    Event { topic: String, payload: Value },
    /// A measured signal crossed a watched condition.
    Signal { name: String, value: f64 },
}

/// The slice of world the provider is allowed to see this step. Assembled by the
/// context family (read-only handles), never written by the provider.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Context {
    /// Ordered, opaque context fragments (retrieved memory, history summary,
    /// world-state query results). The core does not interpret these; the
    /// provider does, in its own idiom.
    pub fragments: Vec<Fragment>,
}

impl Context {
    /// Convenience for context plugins assembling a turn.
    pub fn with(mut self, channel: impl Into<String>, body: impl Into<String>) -> Self {
        self.fragments.push(Fragment { channel: channel.into(), body: body.into() });
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fragment {
    /// e.g. "memory", "history", "world", "persona". Lets a provider pick the
    /// fragments it understands and ignore the rest.
    pub channel: String,
    pub body: String,
}

/// A thing the agent is *able* to do this step. The provider chooses among
/// these; the core's dispatch pipeline is what actually performs them. This is
/// the union of "tools" (LLM), "action nodes" (BT), and "prescribable actions"
/// (rules) — all three are just *named, schema'd capabilities*.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Capability {
    /// Hierarchical id, e.g. "cap.fs.write", "npc.move", "alert.raise".
    pub id: String,
    /// One-line description. Optional for a BT (which keys off id) but used by
    /// an LLM to decide. Empty string is valid — not all providers need it.
    pub summary: String,
    /// JSON Schema for the args this capability accepts. The `validate` stage of
    /// the pipeline enforces it regardless of which provider produced the call,
    /// so a hallucinated-argument LLM and a misconfigured BT fail identically.
    pub args_schema: Value,
}

// ---------------------------------------------------------------------------
// What a provider hands BACK to the core. THE crux type.
// ---------------------------------------------------------------------------

/// The provider's decision for one step: zero or more intents. Crucially this is
/// a *list of effects the provider wants*, not "an LLM response." A tool call is
/// ONE variant, not the whole type — that's what keeps the LLM from being
/// privileged over the behavior tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum ActionIntent {
    /// "Perform this capability with these args." The common case for ALL three
    /// providers: an LLM tool_use, a BT action node firing, a rule's prescribed
    /// action all serialize to exactly this. No provider-specific fields.
    Invoke {
        capability: String,
        args: Value,
        /// Opaque correlation token the provider may set to match a later result
        /// back to this intent. LLMs use it (tool_call_id); a BT can leave it
        /// `None`. Optional, so no one fabricates it.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        correlation: Option<String>,
    },

    // NOTE (v1.0): there is deliberately NO `Mutate` variant. State writes are
    // `Invoke` of a capability with a state-write permission class (e.g.
    // "cap.state_write"). Every argument for a separate Mutate was an argument
    // about GOVERNANCE, which is the pipeline's `govern` stage — not the
    // vocabulary's. Unifying gives the pipeline exactly one effect-path to gate.

    /// "Emit this content to whoever is listening." NOT inherently chat: it's a
    /// game line of dialogue, a chat reply, or an alert body. The channel family
    /// decides how to render it. A pure-control BT step simply never emits this.
    Express { body: String },

    /// "I consider the goal resolved (or abandoned)." Lets the loop terminate
    /// without inspecting provider internals. An LLM signals it via stop_reason;
    /// a BT signals it when the tree returns Success/Failure at the root.
    Conclude { outcome: Outcome },
}

impl ActionIntent {
    /// True for intents that reach the world and therefore MUST pass through the
    /// dispatch pipeline. `Express` and `Conclude` are handled by the loop
    /// directly (channel emit / termination) and are not world-effects in the
    /// governed sense. This predicate is what the loop uses to route intents;
    /// keeping it on the type means a new world-effecting variant can't silently
    /// skip the pipeline — the match here won't compile until it's classified.
    pub fn is_effect(&self) -> bool {
        match self {
            ActionIntent::Invoke { .. } => true,
            ActionIntent::Express { .. } => false,
            ActionIntent::Conclude { .. } => false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Achieved,
    Abandoned,
    /// Still going — the loop should step again. (Default if a provider emits no
    /// Conclude at all, so this is mostly explicit-ness for providers that want
    /// to say "more to do" without other intents.)
    Continue,
}

/// The single trait the core knows. Note what is ABSENT: no messages, no system
/// prompt, no temperature, no model name, no tokens. Those live inside whatever
/// provider needs them.
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn decide(&self, goal: &Goal, ctx: &Context, caps: &[Capability]) -> Decision;
}

/// The full result of one provider decision step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Decision {
    pub intents: Vec<ActionIntent>,
}

impl Decision {
    /// The terminal outcome this decision declares, if any. `None` means the
    /// provider emitted no `Conclude`, which the loop treats as `Continue`.
    pub fn outcome(&self) -> Option<Outcome> {
        self.intents.iter().rev().find_map(|i| match i {
            ActionIntent::Conclude { outcome } => Some(*outcome),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supersession_predicate() {
        let a = Goal { id: "g".into(), revision: 1, objective: "o".into(),
            trigger: Trigger::Tick { sequence: 0 } };
        let b = Goal { revision: 2, ..a.clone() };
        let c = Goal { id: "other".into(), ..b.clone() };
        assert!(a.superseded_by(&b));
        assert!(!b.superseded_by(&a)); // older revision does not supersede
        assert!(!a.superseded_by(&c)); // different identity never supersedes
        assert!(!a.superseded_by(&a)); // equal revision does not supersede
    }

    #[test]
    fn only_invoke_is_a_world_effect() {
        assert!(ActionIntent::Invoke {
            capability: "x".into(), args: Value::Null, correlation: None
        }.is_effect());
        assert!(!ActionIntent::Express { body: "hi".into() }.is_effect());
        assert!(!ActionIntent::Conclude { outcome: Outcome::Achieved }.is_effect());
    }

    #[test]
    fn decision_reports_last_outcome() {
        let d = Decision { intents: vec![
            ActionIntent::Conclude { outcome: Outcome::Continue },
            ActionIntent::Conclude { outcome: Outcome::Achieved },
        ]};
        assert_eq!(d.outcome(), Some(Outcome::Achieved));
        assert_eq!(Decision::default().outcome(), None);
    }

    #[test]
    fn invoke_round_trips_and_omits_none_correlation() {
        let i = ActionIntent::Invoke {
            capability: "alert.raise".into(),
            args: serde_json::json!({"level":"high"}),
            correlation: None,
        };
        let s = serde_json::to_string(&i).unwrap();
        assert!(!s.contains("correlation"), "None correlation must not serialize: {s}");
        let back: ActionIntent = serde_json::from_str(&s).unwrap();
        assert_eq!(i, back);
    }
}
