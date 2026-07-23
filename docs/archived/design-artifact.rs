//! # Pan core vocabulary — the `Goal` / `ActionIntent` contract.
//!
//! This is the make-or-break contract from the spec (§12, first open question):
//! the typed shape that an LLM provider AND a non-LLM provider can both emit
//! *natively*, without either one cosplaying the other.
//!
//! The honesty test for this file is structural: three providers
//! (`provider.llm`, `provider.behaviortree`, `provider.rules`) are implemented
//! at the bottom against the SAME types. If any of them has to fabricate a field
//! that only makes sense for a different provider, the schema has leaked and must
//! be redesigned. See the `leak_test` notes inline.
//!
//! v1.0 (reconciled to the complete report): `ActionIntent` has THREE variants —
//! `Invoke`, `Express`, `Conclude`. State writes are `Invoke` of a state-write
//! capability, not a separate `Mutate`. `Goal` carries a `revision` for the
//! streaming-supersession / abandon-path mechanism.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// What the core hands TO a provider.
// ---------------------------------------------------------------------------

/// A `Goal` is the thing-to-be-pursued this step. It is deliberately NOT a
/// chat message: a chat turn, a cron tick, a game event, and a sensor threshold
/// all reduce to "here is what we're trying to achieve, and what just happened."
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Why this step is happening. Generic over deployment: every `Input` source in
/// the spec collapses into one of these.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Context {
    /// Ordered, opaque context fragments (retrieved memory, history summary,
    /// world-state query results). The core does not interpret these; the
    /// provider does, in its own idiom.
    pub fragments: Vec<Fragment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Achieved,
    Abandoned,
    /// Still going — the loop should step again. (Default if a provider emits no
    /// Conclude at all, so this is mostly explicit-ness for providers that want
    /// to say "more to do" without other intents.)
    Continue,
}

/// Thin alias so the contract doesn't hard-depend on serde_json in its public
/// surface vocabulary (and so a future swap is a one-line change).
pub type Value = serde_json::Value;

/// The full result of one provider decision step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Decision {
    pub intents: Vec<ActionIntent>,
}

/// The single trait the core knows. Note what is ABSENT: no messages, no system
/// prompt, no temperature, no model name, no tokens. Those live inside whatever
/// provider needs them.
pub trait Provider {
    fn id(&self) -> &str;
    fn decide(&self, goal: &Goal, ctx: &Context, caps: &[Capability]) -> Decision;
}

// ===========================================================================
// THE LEAK TEST: three providers, one contract, same file.
// ===========================================================================

/// 1) LLM provider. In reality this maps Goal+Context+caps -> a chat completion,
/// calls a model, and parses tool_use/stop_reason back. Here the model call is
/// stubbed; what matters is that the mapping uses ONLY public contract types and
/// that every chat-shaped detail stays private to this impl.
pub mod provider_llm {
    use super::*;

    pub struct LlmProvider {
        pub model: String, // private chat-shaped detail — never leaks to core
    }

    impl LlmProvider {
        /// Chat-shaped request lives HERE, not in the contract. Proof the LLM
        /// shape is one strategy, not the universal type.
        fn build_prompt(&self, goal: &Goal, ctx: &Context, caps: &[Capability]) -> String {
            let mut p = format!("OBJECTIVE: {}\n", goal.objective);
            if let Trigger::Utterance { from, content } = &goal.trigger {
                p.push_str(&format!("{from} said: {content}\n"));
            }
            for f in &ctx.fragments {
                p.push_str(&format!("[{}] {}\n", f.channel, f.body));
            }
            p.push_str(&format!("You may call: {:?}\n", caps.iter().map(|c| &c.id).collect::<Vec<_>>()));
            p
        }
    }

    impl Provider for LlmProvider {
        fn id(&self) -> &str { "provider.llm" }

        fn decide(&self, goal: &Goal, ctx: &Context, caps: &[Capability]) -> Decision {
            let _prompt = self.build_prompt(goal, ctx, caps);
            // <-- real impl: call model with _prompt, parse tool_use blocks.
            // Stubbed deterministic behavior: greet + remember + conclude.
            Decision {
                intents: vec![
                    ActionIntent::Express { body: "Hello, I can help with that.".into() },
                    // State-write is an Invoke of a capability, not a Mutate.
                    ActionIntent::Invoke {
                        capability: "cap.state_write".into(),
                        args: serde_json::json!({"path": "last_seen", "value": "now"}),
                        correlation: None,
                    },
                    ActionIntent::Conclude { outcome: Outcome::Achieved },
                ],
            }
        }
    }
}

/// 2) Behavior-tree provider. NO language model. It ticks a tree and emits the
/// SAME ActionIntents. The leak test: does it ever set a field that only makes
/// sense for an LLM? It must not. (It leaves `correlation` None and never emits
/// `Express` unless the ticked node is a dialogue node.)
pub mod provider_bt {
    use super::*;

    /// A trivially small tree: each node either invokes a capability or concludes.
    pub enum Node {
        Action { capability: String, args: Value },
        Succeed,
    }

    pub struct BehaviorTreeProvider {
        pub root: Vec<Node>, // ticked in order for this toy version
    }

    impl Provider for BehaviorTreeProvider {
        fn id(&self) -> &str { "provider.behaviortree" }

        fn decide(&self, _goal: &Goal, _ctx: &Context, _caps: &[Capability]) -> Decision {
            // Pure control flow. No prompt, no tokens, no chat. Emits the exact
            // same ActionIntent::Invoke an LLM would — that's the whole point.
            let mut intents = Vec::new();
            for node in &self.root {
                match node {
                    Node::Action { capability, args } => intents.push(ActionIntent::Invoke {
                        capability: capability.clone(),
                        args: args.clone(),
                        correlation: None, // <-- not fabricated. BT has no need.
                    }),
                    Node::Succeed => intents.push(ActionIntent::Conclude { outcome: Outcome::Achieved }),
                }
            }
            Decision { intents }
        }
    }
}

/// 3) Rules engine. Also no model. Fires the first matching rule and emits its
/// prescribed action. Confirms "Invoke" really is the common shape and not a
/// tool-call in disguise: here it's literally the right-hand side of a rule.
pub mod provider_rules {
    use super::*;

    pub struct Rule {
        pub when_signal_over: (String, f64), // (signal name, threshold)
        pub then_invoke: (String, Value),    // (capability id, args)
    }

    pub struct RulesProvider {
        pub rules: Vec<Rule>,
    }

    impl Provider for RulesProvider {
        fn id(&self) -> &str { "provider.rules" }

        fn decide(&self, goal: &Goal, _ctx: &Context, _caps: &[Capability]) -> Decision {
            if let Trigger::Signal { name, value } = &goal.trigger {
                for r in &self.rules {
                    if &r.when_signal_over.0 == name && *value > r.when_signal_over.1 {
                        return Decision {
                            intents: vec![
                                ActionIntent::Invoke {
                                    capability: r.then_invoke.0.clone(),
                                    args: r.then_invoke.1.clone(),
                                    correlation: None,
                                },
                                ActionIntent::Conclude { outcome: Outcome::Achieved },
                            ],
                        };
                    }
                }
            }
            // No rule fired: explicitly say "still going / nothing to do".
            Decision { intents: vec![ActionIntent::Conclude { outcome: Outcome::Continue }] }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests: the leak test made executable.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Vec<Capability> {
        vec![Capability {
            id: "alert.raise".into(),
            summary: "raise an alert".into(),
            args_schema: serde_json::json!({"type":"object"}),
        }]
    }

    #[test]
    fn llm_emits_contract_types_only() {
        let p = provider_llm::LlmProvider { model: "any".into() };
        let g = Goal { id: "g1".into(), revision: 0, objective: "greet".into(),
            trigger: Trigger::Utterance { from: "u".into(), content: "hi".into() } };
        let d = p.decide(&g, &Context::default(), &caps());
        assert!(d.intents.iter().any(|i| matches!(i, ActionIntent::Conclude { .. })));
    }

    #[test]
    fn behavior_tree_emits_identical_invoke_shape() {
        let p = provider_bt::BehaviorTreeProvider {
            root: vec![
                provider_bt::Node::Action { capability: "npc.move".into(), args: serde_json::json!({"to":"door"}) },
                provider_bt::Node::Succeed,
            ],
        };
        let g = Goal { id: "g2".into(), revision: 0, objective: "patrol".into(),
            trigger: Trigger::Tick { sequence: 5 } };
        let d = p.decide(&g, &Context::default(), &[]);
        // The BT's Invoke is byte-identical in shape to an LLM's Invoke.
        match &d.intents[0] {
            ActionIntent::Invoke { capability, correlation, .. } => {
                assert_eq!(capability, "npc.move");
                assert!(correlation.is_none()); // never fabricated
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn rules_invoke_is_the_same_type_as_a_tool_call() {
        let p = provider_rules::RulesProvider {
            rules: vec![provider_rules::Rule {
                when_signal_over: ("temp".into(), 80.0),
                then_invoke: ("alert.raise".into(), serde_json::json!({"level":"high"})),
            }],
        };
        let g = Goal { id: "g3".into(), revision: 0, objective: "watch temp".into(),
            trigger: Trigger::Signal { name: "temp".into(), value: 91.0 } };
        let d = p.decide(&g, &Context::default(), &caps());
        assert_eq!(d.intents[0], ActionIntent::Invoke {
            capability: "alert.raise".into(),
            args: serde_json::json!({"level":"high"}),
            correlation: None,
        });
    }

    #[test]
    fn all_three_are_interchangeable_behind_the_trait() {
        // The core holds them identically. This compiling IS the thesis.
        let providers: Vec<Box<dyn Provider>> = vec![
            Box::new(provider_llm::LlmProvider { model: "x".into() }),
            Box::new(provider_bt::BehaviorTreeProvider { root: vec![provider_bt::Node::Succeed] }),
            Box::new(provider_rules::RulesProvider { rules: vec![] }),
        ];
        for p in &providers {
            let g = Goal { id: "g".into(), revision: 0, objective: "o".into(), trigger: Trigger::Tick { sequence: 1 } };
            let _d: Decision = p.decide(&g, &Context::default(), &[]);
            assert!(!p.id().is_empty());
        }
    }
}
