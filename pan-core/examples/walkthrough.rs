//! A tiny end-to-end demo of the Pan Wave 0 core.
//!
//! Runs each of the three providers (LLM stub, behavior tree, rules) through the
//! identical loop + dispatch pipeline, printing the event stream for each. This
//! is the leak test made visible: the core treats all three the same.
//!
//! Run with: `cargo run --example walkthrough`

use pan_core::loop_engine::{AdmitAll, Once};
use pan_core::prelude::*;

/// An event sink that prints each event as it arrives, on the consumer thread.
struct PrintSink {
    label: String,
}
impl EventSink for PrintSink {
    fn consume(&mut self, event: Event) {
        println!("  [{}] #{:<2} {:?}", self.label, event.seq, event.kind);
    }
}

fn registry() -> CapabilityRegistry {
    let mut r = CapabilityRegistry::new();
    for id in ["cap.state_write", "npc.move", "alert.raise"] {
        r.register(Capability::new(id.to_string(), "", serde_json::json!({ "type": "object" })))
        .expect("unique ids");
    }
    r
}

fn drive(label: &str, provider: &dyn Provider, trigger: Trigger) {
    println!("\n=== {label} ({}) ===", provider.id());
    let reg = registry();
    let (stream, guard) = EventStream::spawn(PrintSink { label: label.into() });
    let pipeline = Pipeline {
        registry: &reg,
        governor: &AllowAll,
        executor: &EchoExecutor,
        events: &stream,
    };
    let lp = Loop {
        provider,
        admitter: &AdmitAll,
        pipeline: &pipeline,
        events: &stream,
    };
    let mut obs = Once(Some(SpanContext {
        persona: PersonaId("demo".into()),
        goal: Goal {
            id: "demo".into(),
            revision: 0,
            objective: "show the core works".into(),
            trigger,
        },
    }));
    let report = lp.run_span(&mut obs, &Context::default());
    stream.shutdown(guard);
    println!(
        "  -> effected={:?} expressed={:?} end={:?}",
        report.effected, report.expressed, report.end
    );
}

fn main() {
    println!("Pan Wave 0 — three providers, one core.\n\
              Each runs through the SAME loop and the SAME non-bypassable pipeline.");

    drive(
        "LLM",
        &pan_core::providers::llm::LlmProvider { model: "demo".into() },
        Trigger::Utterance { from: "user".into(), content: "hello".into() },
    );

    drive(
        "BehaviorTree",
        &pan_core::providers::behaviortree::BehaviorTreeProvider {
            root: vec![
                pan_core::providers::behaviortree::Node::Action {
                    capability: "npc.move".into(),
                    args: serde_json::json!({ "to": "door" }),
                },
                pan_core::providers::behaviortree::Node::Succeed,
            ],
        },
        Trigger::Tick { sequence: 1 },
    );

    drive(
        "Rules",
        &pan_core::providers::rules::RulesProvider {
            rules: vec![pan_core::providers::rules::Rule {
                when: pan_core::providers::rules::Condition::SignalThreshold {
                    name: "temp".into(),
                    op: pan_core::providers::rules::ThresholdOp::Gt,
                    value: 80.0,
                },
                then: pan_core::providers::rules::Action::Invoke {
                    capability: "alert.raise".into(),
                    args: serde_json::json!({ "level": "high" }),
                },
            }],
        },
        Trigger::Signal { name: "temp".into(), value: 91.0 },
    );

    println!("\nThe core could not tell which provider produced each Decision.");
}
