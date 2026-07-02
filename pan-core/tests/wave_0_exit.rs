//! Wave 0 exit test — the manifest's integration-level gate.
//!
//! From the build manifest: *a hand-written integration test drives a stub
//! provider that emits one `Invoke`, through an always-allow govern stage,
//! to a stub capability, and sees the event on the stream.*
//!
//! This is the `--test wave_0_exit` target that the CI wave-0 job runs.
//! It mirrors the inline `wave0_exit_test` in `src/lib.rs` as a proper
//! integration test, so the CI gate is explicit and redundant with the
//! unit test — a regression must break both.

use pan_core::events::{EventKind, EventStream, MemorySink};
use pan_core::loop_engine::{Loop, Once, RunEnd};
use pan_core::pipeline::{AllowAll, EchoExecutor, Pipeline};
use pan_core::registry::CapabilityRegistry;
use pan_core::schema::{
    ActionIntent, Capability, Context, Decision, Goal, Outcome, Provider, Trigger,
};

/// A stub provider that emits exactly one Invoke and then concludes.
struct OneInvoke;
impl Provider for OneInvoke {
    fn id(&self) -> &str {
        "provider.stub"
    }
    fn decide(
        &self,
        _g: &Goal,
        _ctx: &Context,
        _caps: &[pan_core::schema::Capability],
    ) -> Decision {
        Decision {
            intents: vec![
                ActionIntent::Invoke {
                    capability: "stub.cap".into(),
                    args: serde_json::json!({"ok": true}),
                    correlation: Some("corr-1".into()),
                },
                ActionIntent::Conclude {
                    outcome: Outcome::Achieved,
                },
            ],
        }
    }
}

#[test]
fn wave_0_exit() {
    // Register the stub capability so `resolve` binds it.
    let mut reg = CapabilityRegistry::new();
    reg.register(Capability {
        id: "stub.cap".into(),
        summary: "a stub capability".into(),
        args_schema: serde_json::json!({"type": "object"}),
    })
    .unwrap();

    // Wire the event stream with an in-memory sink we can inspect.
    let sink = MemorySink::new();
    let events_handle = sink.handle();
    let mut stream = EventStream::spawn(sink);

    // Always-allow govern stage + echo executor = trivial end-to-end path.
    let pipeline = Pipeline {
        registry: &reg,
        governor: &AllowAll,
        executor: &EchoExecutor,
        events: &stream,
    };

    // Drive one discrete span.
    let provider = OneInvoke;
    let lp = Loop {
        provider: &provider,
        pipeline: &pipeline,
        events: &stream,
    };
    let mut obs = Once(Some(Goal {
        id: "run-1".into(),
        revision: 0,
        objective: "do the thing".into(),
        trigger: Trigger::Tick { sequence: 1 },
    }));
    let report = lp.run_span(&mut obs, &Context::default());

    // The effect executed and the span concluded cleanly.
    assert_eq!(report.effected, vec!["stub.cap"]);
    assert_eq!(report.end, Some(RunEnd::Concluded(Outcome::Achieved)));

    // Close the stream and join the consumer so all events are collected.
    stream.shutdown();

    // "...and sees the event on the stream." Assert the Effected event landed.
    let events = events_handle.lock().unwrap();
    let saw_effected = events.iter().any(|e| {
        matches!(
            &e.kind,
            EventKind::Effected {
                capability, ..
            } if capability == "stub.cap"
        )
    });
    assert!(saw_effected, "the Effected event must appear on the stream");

    // Sequence numbers are dense and ordered (the stream's ordering guarantee).
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e.seq, i as u64);
    }
}
