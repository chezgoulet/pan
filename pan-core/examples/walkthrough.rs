//! A tiny end-to-end demo of the Pan Wave 0 core.
//!
//! Runs each of the three providers (LLM stub, behavior tree, rules) through the
//! identical loop + dispatch pipeline, printing the event stream for each. This
//! is the leak test made visible: the core treats all three the same.
//!
//! Run with: `cargo run --example walkthrough`

use pan_core::prelude::*;
use pan_core::providers::{behaviortree, llm, rules};

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
        r.register(Capability {
            id: id.into(),
            summary: String::new(),
            args_schema: serde_json::json!({ "type": "object" }),
        })
        .expect("unique ids");
    }
    r
}

async fn drive(label: &str, provider: &dyn Provider, trigger: Trigger) {
    println!("\n=== {label} ({}) ===", provider.id());
    let reg = registry();
    let mut stream = EventStream::spawn(PrintSink {
        label: label.into(),
    });
    let pipeline = Pipeline {
        registry: &reg,
        governor: &AllowAll,
        executor: &EchoExecutor,
        events: &stream,
    };
    let lp = Loop {
        provider,
        pipeline: &pipeline,
        events: &stream,
        scope: Scope::system(),
        token_tx: None,
    };
    let mut obs = Once(Some(Goal {
        id: "demo".into(),
        revision: 0,
        objective: "show the core works".into(),
        trigger,
    }));
    let report = lp.run_span(&mut obs, &Context::default()).await;
    stream.shutdown();
    println!(
        "  -> effected={:?} expressed={:?} end={:?}",
        report.effected, report.expressed, report.end
    );
}

#[tokio::main]
async fn main() {
    println!(
        "Pan Wave 0 — three providers, one core.\n\
              Each runs through the SAME loop and the SAME non-bypassable pipeline."
    );

    drive(
        "LLM",
        &llm::LlmProvider {
            model: "demo".into(),
        },
        Trigger::Utterance {
            from: "user".into(),
            content: "hello".into(),
        },
    )
    .await;

    drive(
        "BehaviorTree",
        &behaviortree::BehaviorTreeProvider {
            root: vec![
                behaviortree::Node::Action {
                    capability: "npc.move".into(),
                    args: serde_json::json!({ "to": "door" }),
                },
                behaviortree::Node::Succeed,
            ],
        },
        Trigger::Tick { sequence: 1 },
    )
    .await;

    drive(
        "Rules",
        &rules::RulesProvider {
            rules: vec![rules::Rule {
                when_signal_over: Some(("temp".into(), 80.0)),
                when_event_topic: None,
                then_invoke: ("alert.raise".into(), serde_json::json!({ "level": "high" })),
            }],
        },
        Trigger::Signal {
            name: "temp".into(),
            value: 91.0,
        },
    )
    .await;

    println!("\nThe core could not tell which provider produced each Decision.");
}
