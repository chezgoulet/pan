//! # Pan core — Wave 0.
//!
//! The irreducible core that plugins plug into. Per the build manifest, Wave 0
//! is the five core responsibilities' substrate with **no real plugins**:
//!
//! - [`schema`] — the `Goal` / `ActionIntent` vocabulary (the make-or-break
//!   contract; validated by the three-provider leak test in [`providers`]).
//! - [`pipeline`] — the non-bypassable dispatch pipeline
//!   `resolve -> validate -> govern -> execute -> record`, where the unsafe path
//!   (execute without a passing govern) *does not compile*.
//! - [`loop_engine`] — the `observe -> decide -> enact -> commit` loop, stream-
//!   driven, with the abandon-path for superseded goals.
//! - [`events`] — the ordered, typed, off-thread event stream.
//! - [`registry`] — the capability registry and the Caddy-style plugin
//!   lifecycle (conflict = error, never last-wins).
//! - [`handles`] — scoped capability handles; the read-only grant that *cannot*
//!   write, enforced by the type system.
//!
//! What is deliberately ABSENT from the core: prompts, tokens, models, chat
//! messages, tool-call conventions. Those live inside the `provider.llm` plugin.
//!
//! ## The Wave 0 exit test
//!
//! From the manifest: *a hand-written integration test drives a stub provider
//! that emits one `Invoke`, through an always-allow govern stage, to a stub
//! capability, and sees the event on the stream.* That test is
//! [`tests::wave0_exit_test`] below.

pub mod events;
pub mod handles;
pub mod loop_engine;
pub mod pipeline;
pub mod plugins;
pub mod providers;
pub mod providers_litellm;
pub mod providers_llm;
pub mod registry;
pub mod schema;
pub mod state;

// A small, curated public prelude so downstream plugin crates have one import.
pub mod prelude {
    pub use crate::events::{Event, EventKind, EventSink, EventStream, MemorySink, StageStatus};
    pub use crate::handles::{Fact, MemoryQuery, MemoryStore, Query};
    pub use crate::loop_engine::{AdmitAll, Admitter, Loop, Observations, Once, RunEnd, RunReport};
    pub use crate::pipeline::{
        AllowAll, EchoExecutor, EffectRequest, Executor, Governor, Pipeline, PipelineError, Verdict,
    };
    pub use crate::registry::{
        CapabilityRegistry, ConflictError, Lifecycle, LifecycleError, Plugin, PluginError,
    };
    pub use crate::schema::{
        ActionIntent, Capability, Context, Decision, Fragment, Goal, Outcome, Provider, Trigger,
        Value,
    };
}

#[cfg(test)]
mod tests {
    use crate::events::{EventKind, EventStream, MemorySink};
    use crate::loop_engine::{AdmitAll, Loop, Once, RunEnd};
    use crate::pipeline::{AllowAll, EchoExecutor, Pipeline};
    use crate::providers::{behaviortree, llm, rules};
    use crate::registry::CapabilityRegistry;
    use crate::schema::{
        ActionIntent, Capability, Context, Decision, Goal, Outcome, PersonaId, Provider, SpanContext, Trigger,
    };

    /// THE WAVE 0 EXIT TEST (build manifest):
    /// "a hand-written integration test drives a stub provider that emits one
    /// `Invoke`, through an always-allow govern stage, to a stub capability, and
    /// sees the event on the stream."
    #[test]
    fn wave0_exit_test() {
        // A stub provider that emits exactly one Invoke and then concludes.
        struct OneInvoke;
        impl Provider for OneInvoke {
            fn id(&self) -> &str { "provider.stub" }
            fn decide(&self, _g: &Goal, _c: &Context, _caps: &[Capability]) -> Decision {
                Decision { intents: vec![
                    ActionIntent::Invoke {
                        capability: "stub.cap".into(),
                        args: serde_json::json!({"ok": true}),
                        correlation: Some("corr-1".into()),
                    },
                    ActionIntent::Conclude { outcome: Outcome::Achieved },
                ]}
            }
        }

        // Register the stub capability so `resolve` binds it.
        let mut reg = CapabilityRegistry::new();
        reg.register(Capability {
            id: "stub.cap".into(),
            summary: "a stub capability".into(),
            args_schema: serde_json::json!({"type": "object"}),
        }).unwrap();

        // Wire the event stream with an in-memory sink we can inspect.
        let sink = MemorySink::new();
        let events_handle = sink.handle();
        let (stream, guard) = EventStream::spawn(sink);

        // Always-allow govern stage + echo executor = the trivial end-to-end path.
        let pipeline = Pipeline {
            registry: &reg,
            governor: &AllowAll,
            executor: &EchoExecutor,
            events: &stream,
        };

        // Drive one discrete span.
        let provider = OneInvoke;
        let lp = Loop { provider: &provider, admitter: &AdmitAll, pipeline: &pipeline, events: &stream };
        let mut obs = Once(Some(SpanContext {
            persona: PersonaId("default".into()),
            goal: Goal {
                id: "run-1".into(), revision: 0, objective: "do the thing".into(),
                trigger: Trigger::Tick { sequence: 1 },
            },
        }));
        let report = lp.run_span(&mut obs, &Context::default());

        // The effect executed and the span concluded cleanly.
        assert_eq!(report.effected, vec!["stub.cap"]);
        assert_eq!(report.end, Some(RunEnd::Concluded(Outcome::Achieved)));

        // Close the stream and join the consumer so all events are collected.
        stream.shutdown(guard);

        // "...and sees the event on the stream." Assert the Effected event landed.
        let events = events_handle.lock().unwrap();
        let saw_effected = events.iter().any(|e| matches!(
            &e.kind, EventKind::Effected { capability, .. } if capability == "stub.cap"
        ));
        assert!(saw_effected, "the Effected event must appear on the stream");

        // Sequence numbers are dense and ordered (the stream's ordering guarantee).
        for (i, e) in events.iter().enumerate() {
            assert_eq!(e.seq, i as u64);
        }
    }

    /// The leak-test thesis, made executable at the crate root: all three
    /// providers are held identically behind the trait. Its compiling IS the
    /// thesis; the assertions just exercise it.
    #[test]
    fn all_three_are_interchangeable_behind_the_trait() {
        let providers: Vec<Box<dyn Provider>> = vec![
            Box::new(llm::LlmProvider { model: "x".into() }),
            Box::new(behaviortree::BehaviorTreeProvider {
                root: vec![behaviortree::Node::Succeed],
            }),
            Box::new(rules::RulesProvider { rules: vec![] }),
        ];
        for p in &providers {
            let g = Goal { id: "g".into(), revision: 0, objective: "o".into(),
                trigger: Trigger::Tick { sequence: 1 } };
            let _d: Decision = p.decide(&g, &Context::default(), &[]);
            assert!(!p.id().is_empty());
        }
    }

    /// All three providers drive the SAME loop + pipeline with no per-provider
    /// special-casing — the integration-level version of the leak test.
    #[test]
    fn all_three_drive_the_same_pipeline() {
        let mut reg = CapabilityRegistry::new();
        reg.register(Capability { id: "cap.state_write".into(), summary: "".into(),
            args_schema: serde_json::json!({"type":"object"}) }).unwrap();
        reg.register(Capability { id: "npc.move".into(), summary: "".into(),
            args_schema: serde_json::json!({"type":"object"}) }).unwrap();
        reg.register(Capability { id: "alert.raise".into(), summary: "".into(),
            args_schema: serde_json::json!({"type":"object"}) }).unwrap();

        let providers: Vec<Box<dyn Provider>> = vec![
            Box::new(llm::LlmProvider { model: "x".into() }),
            Box::new(behaviortree::BehaviorTreeProvider { root: vec![
                behaviortree::Node::Action { capability: "npc.move".into(), args: serde_json::json!({}) },
                behaviortree::Node::Succeed,
            ]}),
            Box::new(rules::RulesProvider { rules: vec![rules::Rule {
                when: rules::Condition::SignalThreshold {
                    name: "temp".into(),
                    op: rules::ThresholdOp::Gt,
                    value: 80.0,
                },
                then: rules::Action::Invoke {
                    capability: "alert.raise".into(),
                    args: serde_json::json!({}),
                },
            }]}),
        ];

        // A trigger that satisfies all three (the rules provider needs a Signal).
        for p in &providers {
            let (stream, guard) = EventStream::spawn(MemorySink::new());
            let pipeline = Pipeline { registry: &reg, governor: &AllowAll,
                executor: &EchoExecutor, events: &stream };
            let lp = Loop { provider: p.as_ref(), admitter: &AdmitAll, pipeline: &pipeline, events: &stream };
            let mut obs = Once(Some(SpanContext {
                persona: PersonaId("default".into()),
                goal: Goal { id: "g".into(), revision: 0, objective: "o".into(),
                    trigger: Trigger::Signal { name: "temp".into(), value: 91.0 } },
            }));
            let report = lp.run_span(&mut obs, &Context::default());
            assert!(report.end.is_some(), "provider {} produced no terminal state", p.id());
            stream.shutdown(guard);
        }
    }
}
