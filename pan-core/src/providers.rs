//! # The leak test: three providers, one contract.
//!
//! This is the honesty check from the README and synthesis §2.2/§12. Three
//! providers — an LLM, a behavior tree, and a rules engine — implement the SAME
//! [`Provider`] trait against the SAME [`ActionIntent`] vocabulary. If any of
//! them must fabricate a field that only makes sense for another, the schema has
//! leaked. The test `all_three_are_interchangeable_behind_the_trait` (in the
//! crate root) holding all three in a `Vec<Box<dyn Provider>>` and compiling IS
//! the thesis.

use crate::schema::{ActionIntent, Capability, Context, Decision, Goal, Outcome, Trigger, Value};

/// 1) LLM provider. In reality this maps Goal+Context+caps -> a chat completion,
/// calls a model, and parses tool_use/stop_reason back. Here the model call is
/// stubbed; what matters is that the mapping uses ONLY public contract types and
/// that every chat-shaped detail stays private to this impl.
pub mod llm {
    use super::*;
    use crate::schema::Provider;

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
            p.push_str(&format!(
                "You may call: {:?}\n",
                caps.iter().map(|c| &c.id).collect::<Vec<_>>()
            ));
            p
        }
    }

    impl Provider for LlmProvider {
        fn id(&self) -> &str {
            "provider.llm"
        }

        fn decide(&self, goal: &Goal, ctx: &Context, caps: &[Capability]) -> Decision {
            let _prompt = self.build_prompt(goal, ctx, caps);
            // <-- real impl: call model with _prompt, parse tool_use blocks.
            // Stubbed deterministic behavior: greet + remember + conclude.
            Decision {
                intents: vec![
                    ActionIntent::Express {
                        body: "Hello, I can help with that.".into(),
                    },
                    // State-write is an Invoke of a capability, not a Mutate.
                    ActionIntent::Invoke {
                        capability: "cap.state_write".into(),
                        args: serde_json::json!({"path": "last_seen", "value": "now"}),
                        correlation: None,
                    },
                    ActionIntent::Conclude {
                        outcome: Outcome::Achieved,
                    },
                ],
            }
        }
    }
}

/// 2) Behavior-tree provider. NO language model. It ticks a tree and emits the
/// SAME ActionIntents. The leak test: does it ever set a field that only makes
/// sense for an LLM? It must not. (It leaves `correlation` None and never emits
/// `Express` unless the ticked node is a dialogue node.)
pub mod behaviortree {
    use super::*;
    use crate::schema::Provider;

    /// A trivially small tree: each node either invokes a capability or concludes.
    pub enum Node {
        Action { capability: String, args: Value },
        Succeed,
    }

    pub struct BehaviorTreeProvider {
        pub root: Vec<Node>, // ticked in order for this toy version
    }

    impl Provider for BehaviorTreeProvider {
        fn id(&self) -> &str {
            "provider.behaviortree"
        }

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
                    Node::Succeed => intents.push(ActionIntent::Conclude {
                        outcome: Outcome::Achieved,
                    }),
                }
            }
            Decision { intents }
        }
    }
}

/// 3) Rules engine. Also no model. Fires the first matching rule and emits its
/// prescribed action. Confirms "Invoke" really is the common shape and not a
/// tool-call in disguise: here it's literally the right-hand side of a rule.
pub mod rules {
    use super::*;
    use crate::schema::Provider;

    pub struct Rule {
        pub when_signal_over: (String, f64), // (signal name, threshold)
        pub then_invoke: (String, Value),    // (capability id, args)
    }

    pub struct RulesProvider {
        pub rules: Vec<Rule>,
    }

    impl Provider for RulesProvider {
        fn id(&self) -> &str {
            "provider.rules"
        }

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
                                ActionIntent::Conclude {
                                    outcome: Outcome::Achieved,
                                },
                            ],
                        };
                    }
                }
            }
            // No rule fired: explicitly say "still going / nothing to do".
            Decision {
                intents: vec![ActionIntent::Conclude {
                    outcome: Outcome::Continue,
                }],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Provider;

    fn caps() -> Vec<Capability> {
        vec![Capability {
            id: "alert.raise".into(),
            summary: "raise an alert".into(),
            args_schema: serde_json::json!({"type":"object"}),
        }]
    }

    #[test]
    fn llm_emits_contract_types_only() {
        let p = llm::LlmProvider {
            model: "any".into(),
        };
        let g = Goal {
            id: "g1".into(),
            revision: 0,
            objective: "greet".into(),
            trigger: Trigger::Utterance {
                from: "u".into(),
                content: "hi".into(),
            },
        };
        let d = p.decide(&g, &Context::default(), &caps());
        assert!(d
            .intents
            .iter()
            .any(|i| matches!(i, ActionIntent::Conclude { .. })));
    }

    #[test]
    fn behavior_tree_emits_identical_invoke_shape() {
        let p = behaviortree::BehaviorTreeProvider {
            root: vec![
                behaviortree::Node::Action {
                    capability: "npc.move".into(),
                    args: serde_json::json!({"to":"door"}),
                },
                behaviortree::Node::Succeed,
            ],
        };
        let g = Goal {
            id: "g2".into(),
            revision: 0,
            objective: "patrol".into(),
            trigger: Trigger::Tick { sequence: 5 },
        };
        let d = p.decide(&g, &Context::default(), &[]);
        match &d.intents[0] {
            ActionIntent::Invoke {
                capability,
                correlation,
                ..
            } => {
                assert_eq!(capability, "npc.move");
                assert!(correlation.is_none()); // never fabricated
            }
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn rules_invoke_is_the_same_type_as_a_tool_call() {
        let p = rules::RulesProvider {
            rules: vec![rules::Rule {
                when_signal_over: ("temp".into(), 80.0),
                then_invoke: ("alert.raise".into(), serde_json::json!({"level":"high"})),
            }],
        };
        let g = Goal {
            id: "g3".into(),
            revision: 0,
            objective: "watch temp".into(),
            trigger: Trigger::Signal {
                name: "temp".into(),
                value: 91.0,
            },
        };
        let d = p.decide(&g, &Context::default(), &caps());
        assert_eq!(
            d.intents[0],
            ActionIntent::Invoke {
                capability: "alert.raise".into(),
                args: serde_json::json!({"level":"high"}),
                correlation: None,
            }
        );
    }
}
