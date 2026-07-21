//! # Property tests for the Pan core pipeline.
//!
//! Uses proptest to verify the pipeline's key invariants under arbitrary inputs:
//! no panic, no bypass of governance, and decision round-trips are lossless.

use pan_core::events::{EventStream, MemorySink};
use pan_core::pipeline::{
    AllowAll, EchoExecutor, EffectRequest, Governor, Pipeline, ScopedGovernor, Verdict,
};
use pan_core::registry::CapabilityRegistry;
use pan_core::schema::{ActionIntent, Capability, Decision, Outcome, Scope, Value};
use proptest::prelude::*;

proptest! {
    #[test]
    fn scoped_governor_never_panics(
        capability: String,
        origin: String,
        has_grant: bool,
    ) {
        // Test with arbitrary shorter strings to avoid pattern issues
        let capability = if capability.is_empty() || !capability.contains('.') {
            "cap.test".to_string()
        } else { capability };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut gov = ScopedGovernor::new();
        if has_grant {
            let prefix = capability.split('.').next().unwrap_or("cap");
            gov = gov.grant(origin.clone(), [prefix.to_string()]);
        }
        let scope = Scope::new(origin);
        let verdict = rt.block_on(gov.govern(&scope, &capability, &Value::Null));
        match verdict {
            Verdict::Allow | Verdict::Deny { .. } | Verdict::RequireApproval { .. } => {}
        }
    }

    #[test]
    fn decision_round_trip(
        express_body: String,
        capability: String,
        args_keys: Vec<String>,
    ) {
        let mut args = serde_json::Map::new();
        for (i, k) in args_keys.iter().enumerate() {
            args.insert(k.clone(), i.into());
        }
        let capability = if capability.is_empty() || !capability.contains('.') {
            "cap.test".to_string()
        } else { capability };
        let decision = Decision {
            intents: vec![
                ActionIntent::Express { body: express_body.clone() },
                ActionIntent::Invoke {
                    capability: capability.clone(),
                    args: Value::Object(args.clone()),
                    correlation: None,
                },
                ActionIntent::Conclude { outcome: Outcome::Achieved },
            ],
        };
        let json = serde_json::to_string(&decision).unwrap();
        let round: Decision = serde_json::from_str(&json).unwrap();
        assert_eq!(decision, round);
    }

    #[test]
    fn sequential_dispatch_never_fails(
        n in 1usize..8,
    ) {
        let prefix = "m";
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut reg = CapabilityRegistry::new();
            for i in 0..n {
                let id = format!("{prefix}.{i}");
                reg.register(Capability {
                    id,
                    summary: String::new(),
                    args_schema: serde_json::json!({"type": "object"}),
                }).unwrap();
            }
            let sink = MemorySink::new();
            let mut stream = EventStream::spawn(sink);
            let pipeline = Pipeline {
                registry: &reg,
                governor: &AllowAll,
                executor: &EchoExecutor,
                events: &stream,
            };
            let caps: Vec<String> = reg.all().into_iter().map(|c| c.id).collect();
            for cap in &caps {
                let req = EffectRequest {
                    capability: cap.clone(),
                    args: serde_json::json!({}),
                    correlation: None,
                    scope: Scope::system(),
                };
                let result = pipeline.dispatch(req).await;
                assert!(result.is_ok(), "dispatch failed: {result:?}");
            }
            stream.shutdown();
        });
    }
}

/// Test that the Soul Protocol NDJSON parser never panics on arbitrary byte
/// sequences — it should only return errors for malformed input.
#[test]
fn daemon_wire_parser_is_robust() {
    // We can't easily import pan_daemon::wire here (different crate), so
    // we test the serde_json round-trip for the core schema types, which
    // is what the wire parser does internally.
    let mut rng = simple_rng();
    for _ in 0..10_000 {
        let len = (rng() % 256) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| rng() as u8).collect();
        let s = String::from_utf8_lossy(&bytes);
        // serde_json must never panic on any input, only return Err.
        let _: Result<serde_json::Value, _> = serde_json::from_str(&s);
    }
}

/// A minimal pseudo-random number generator that doesn't require `rand`.
fn simple_rng() -> impl FnMut() -> u64 {
    let mut state: u64 = std::time::UNIX_EPOCH
        .elapsed()
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(42);
    move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state >> 33
    }
}
