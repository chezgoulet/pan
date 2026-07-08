//! # Per-decision latency benchmarks
//!
//! Measures time from observation entering the loop to Decision being emitted.
//!
//! - **Fast path:** stub provider (ScriptedProvider) — deterministic, no string
//!   building, measures the loop + pipeline core overhead.
//! - **Slow path:** `LlmProvider` — builds a prompt string from Goal + Context +
//!   Caps, representative of the LLM-shaped provider path even without a real
//!   model call.
//! - **Multi-intent decision:** a decision with 1, 5, or 10 Invoke intents to
//!   measure how the loop + pipeline scales with intent count.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use pan_core::events::{EventStream, MemorySink};
use pan_core::loop_engine::{Loop, Once};
use pan_core::pipeline::{AllowAll, EchoExecutor, Pipeline};
use pan_core::registry::CapabilityRegistry;
use pan_core::schema::{
    ActionIntent, Capability, Context, Decision, Goal, Outcome, Provider, Trigger,
};

// ---------------------------------------------------------------------------
// Fast-path provider: returns a fixed decision instantly (no string building).
// ---------------------------------------------------------------------------
struct ScriptedProvider(Decision);

impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "provider.scripted"
    }
    fn decide(&self, _g: &Goal, _c: &Context, _caps: &[Capability]) -> Decision {
        self.0.clone()
    }
}

fn make_goal() -> Goal {
    Goal {
        id: "bench-g".into(),
        revision: 0,
        objective: "benchmark objective".into(),
        trigger: Trigger::Tick { sequence: 1 },
    }
}

fn make_context(n_fragments: usize) -> Context {
    let mut ctx = Context::default();
    for i in 0..n_fragments {
        ctx = ctx.with(format!("ch.{i}"), format!("fragment body content {i}"));
    }
    ctx
}

fn make_registry(n_caps: usize) -> CapabilityRegistry {
    let mut reg = CapabilityRegistry::new();
    for i in 0..n_caps {
        let _ = reg.register(Capability::new(format!("cap.bench.{i}"), "", serde_json::json!({"type": "object"})));
    }
    reg
}

fn make_pipeline<'a>(
    reg: &'a CapabilityRegistry,
    events: &'a EventStream,
) -> Pipeline<'a> {
    Pipeline {
        registry: reg,
        governor: &AllowAll,
        executor: &EchoExecutor,
        events,
    }
}

fn make_provider_intents(n_invokes: usize) -> Decision {
    let mut intents: Vec<ActionIntent> = (0..n_invokes)
        .map(|i| ActionIntent::Invoke {
            capability: format!("cap.bench.{i}"),
            args: serde_json::json!({"key": "value"}),
            correlation: None,
        })
        .collect();
    intents.push(ActionIntent::Conclude {
        outcome: Outcome::Achieved,
    });
    Decision { intents }
}

// ---------------------------------------------------------------------------
// LlmProvider — the slow path. Constructor is pub but module is crate::providers
// which re-exports from crate::providers::llm via crate::schema. Actually in the
// code the LlmProvider lives in pan_core::providers::llm.
// ---------------------------------------------------------------------------
use pan_core::providers::llm::LlmProvider;

// ---------------------------------------------------------------------------
// Benchmark: fast-path, single invoke
// ---------------------------------------------------------------------------
fn bench_fast_path_single(c: &mut Criterion) {
    let reg = make_registry(1);
    let ctx = make_context(0);
    let provider = ScriptedProvider(make_provider_intents(1));

    let mut group = c.benchmark_group("latency/fast-path");
    group.sample_size(100);
    group.measurement_time(std::time::Duration::from_secs(5));

    group.bench_function("single-invoke", |b| {
        b.iter(|| {
            let (stream, guard) = EventStream::spawn(MemorySink::new());
            let pipeline = make_pipeline(&reg, &stream);
            let lp = Loop {
                provider: &provider,
                pipeline: &pipeline,
                events: &stream,
            };
            let mut obs = Once(Some(black_box(make_goal())));
            let report = lp.run_span(&mut obs, &ctx);
            stream.shutdown(guard);
            black_box(report)
        })
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: fast-path, multi-invoke (1, 5, 10 intents)
// ---------------------------------------------------------------------------
fn bench_fast_path_multi(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency/fast-path");
    group.sample_size(100);
    group.measurement_time(std::time::Duration::from_secs(5));

    for n_invokes in [1usize, 5, 10] {
        let reg = make_registry(n_invokes);
        let ctx = make_context(0);
        let provider = ScriptedProvider(make_provider_intents(n_invokes));

        group.bench_function(format!("{n_invokes}-invokes"), |b| {
            b.iter(|| {
                let (stream, guard) = EventStream::spawn(MemorySink::new());
                let pipeline = make_pipeline(&reg, &stream);
                let lp = Loop {
                    provider: &provider,
                    pipeline: &pipeline,
                    events: &stream,
                };
                let mut obs = Once(Some(black_box(make_goal())));
                let report = lp.run_span(&mut obs, &ctx);
                stream.shutdown(guard);
                black_box(report)
            })
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: slow path — LlmProvider (prompt building overhead)
// ---------------------------------------------------------------------------
fn bench_slow_path(c: &mut Criterion) {
    let reg = make_registry(1);
    let ctx = make_context(3); // a few context fragments
    let llm = LlmProvider {
        model: "bench-model".into(),
    };

    let mut group = c.benchmark_group("latency/slow-path");
    group.sample_size(100);
    group.measurement_time(std::time::Duration::from_secs(5));

    group.bench_function("llm-provider", |b| {
        b.iter(|| {
            let (stream, guard) = EventStream::spawn(MemorySink::new());
            let pipeline = make_pipeline(&reg, &stream);
            let lp = Loop {
                provider: &llm,
                pipeline: &pipeline,
                events: &stream,
            };
            let mut obs = Once(Some(black_box(make_goal())));
            let report = lp.run_span(&mut obs, &ctx);
            stream.shutdown(guard);
            black_box(report)
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_fast_path_single,
    bench_fast_path_multi,
    bench_slow_path,
);
criterion_main!(benches);
