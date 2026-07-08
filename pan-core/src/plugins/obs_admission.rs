//! # `obs.admission` — heartbeat admission filter (Wave 5).
//!
//! Most [`Trigger::Tick`] observations should never reach the provider — they
//! fire every N minutes but nothing meaningful changed. This plugin wraps an
//! [`Observations`] source and drops ticks that do not pass admission.
//!
//! ## Mechanism
//!
//! 1. Get the next [`SpanContext`] from the inner observations source.
//! 2. If it is a **non-Tick** trigger (utterance, signal, event): always admit.
//! 3. If it is a **Tick**:
//!    - **No Persona binding** (empty/generic `persona`): reject.
//!    - **Min interval** since last admitted tick for this Persona not elapsed
//!      AND **state unchanged** (same objective for the same Persona): drop.
//!    - Otherwise: admit and record the new state snapshot.
//!
//! ## Per-Persona semantics (from issue #47)
//!
//! Admission state is tracked per `PersonaId`. Persona A's tick never wakes
//! Persona B if nothing changed for B. A tick whose persona is empty or the
//! literal sentinel `"none"` is rejected at admission.
//!
//! ## Configurable interval
//!
//! `min_interval_ms` controls the minimum wall-clock time between admitted
//! ticks for the same Persona. Default: 60_000 ms (1 minute).

use crate::loop_engine::Observations;
use crate::registry::{Plugin, PluginError};
use crate::schema::{Goal, PersonaId, SpanContext, Trigger};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

/// Persona-specific state tracked by the admission filter.
#[derive(Clone, Debug)]
struct PersonaState {
    /// Wall-clock instant of the last admitted tick for this Persona.
    last_admitted: Instant,
    /// Hash of (persona, objective) at the time of last admission.
    last_state_hash: u64,
}

// ---------------------------------------------------------------------------
// AdmissionFilter — wraps any Observations source
// ---------------------------------------------------------------------------

/// Filters Tick observations from an inner [`Observations`] source.
///
/// Only forwards ticks that pass admission; silently drops the rest.
/// Non-Tick triggers always pass through.
///
/// ## Type parameter
/// - `O`: the inner observations source (e.g. [`Once`], a channel receiver, etc.)
pub struct AdmissionFilter<O: Observations> {
    inner: O,
    min_interval: Duration,
    /// Per-Persona state, keyed by `PersonaId`.
    persona_state: HashMap<String, PersonaState>,
}

impl<O: Observations> AdmissionFilter<O> {
    /// Create a new admission filter.
    ///
    /// `min_interval_ms` — minimum wall-clock milliseconds between admitted
    /// ticks for the same Persona. Set to 0 to admit every tick.
    pub fn new(inner: O, min_interval_ms: u64) -> Self {
        Self {
            inner,
            min_interval: Duration::from_millis(min_interval_ms),
            persona_state: HashMap::new(),
        }
    }

    /// Build a stable hash of the admission-relevant state for a span.
    ///
    /// Only the Persona identity and the objective are hashed.
    /// The trigger sequence number is deliberately excluded — a new tick
    /// sequence alone does not constitute a "state change".
    fn state_hash(span: &SpanContext) -> u64 {
        let mut hasher = DefaultHasher::new();
        span.persona.as_str().hash(&mut hasher);
        span.goal.objective.hash(&mut hasher);
        hasher.finish()
    }

    /// Return `true` if the span passes admission.
    fn should_admit(&mut self, span: &SpanContext) -> bool {
        // Non-Tick triggers are always admitted — utterance, signal, and
        // event triggers indicate live interaction, not a cron heartbeat.
        match &span.goal.trigger {
            Trigger::Utterance { .. } | Trigger::Signal { .. } | Trigger::Event { .. } => {
                return true;
            }
            Trigger::Tick { .. } => { /* check admission below */ }
        }

        // --- Tick admission logic ---

        // A tick without a Persona binding is rejected at admission
        // (delta requirement: "tick without Persona binding is rejected").
        let persona_str = span.persona.as_str();
        if persona_str.is_empty() || persona_str == "none" {
            return false;
        }

        let hash = Self::state_hash(span);

        // Check per-Persona admission state.
        if let Some(state) = self.persona_state.get(persona_str) {
            // If the minimum interval has NOT elapsed AND the state hash
            // is unchanged, this is a cheap observation — drop it.
            if Instant::now().duration_since(state.last_admitted) < self.min_interval
                && hash == state.last_state_hash
            {
                return false;
            }
        }

        // Passing admission — record the new state snapshot.
        self.persona_state.insert(
            persona_str.to_string(),
            PersonaState {
                last_admitted: Instant::now(),
                last_state_hash: hash,
            },
        );
        true
    }

    /// Access the inner observations source (for inspection in tests).
    pub fn inner(&self) -> &O {
        &self.inner
    }

    /// Access the inner observations source mutably.
    pub fn inner_mut(&mut self) -> &mut O {
        &mut self.inner
    }

    /// Number of Personas currently tracked.
    pub fn tracked_personas(&self) -> usize {
        self.persona_state.len()
    }
}

impl<O: Observations> Observations for AdmissionFilter<O> {
    fn next_goal(&mut self) -> Option<SpanContext> {
        // Keep pulling from the inner source until we find an admissible
        // span or the source is truly exhausted.
        while let Some(span) = self.inner.next_goal() {
            if self.should_admit(&span) {
                return Some(span);
            }
            // Dropped — silently skip, try the next one.
        }
        None
    }

    fn superseding(&mut self, current: &SpanContext) -> Option<SpanContext> {
        // Delegate supersession detection to the inner source.
        self.inner.superseding(current)
    }
}

// ---------------------------------------------------------------------------
// AdmissionPlugin — lifecycle-managed plugin wrapper
// ---------------------------------------------------------------------------

/// The heartbeat admission plugin for lifecycle management.
///
/// Holds configuration and can be registered with the plugin [`Lifecycle`].
/// The actual filtering logic lives in [`AdmissionFilter`]; this plugin
/// provides the config that `AdmissionFilter` reads.
pub struct AdmissionPlugin {
    /// Minimum interval between admitted ticks (milliseconds).
    pub min_interval_ms: u64,
}

impl AdmissionPlugin {
    /// Create a new admission plugin with the given minimum interval.
    ///
    /// `min_interval_ms`: minimum wall-clock milliseconds between admitted
    /// ticks for the same Persona. The idle-skip loop typically sets this to
    /// 30_000–300_000 (30 s – 5 min).
    pub fn new(min_interval_ms: u64) -> Self {
        Self { min_interval_ms }
    }
}

impl Plugin for AdmissionPlugin {
    fn id(&self) -> &str {
        "obs.admission"
    }

    fn provision(&mut self) -> Result<(), PluginError> {
        // Validate config: interval must be non-negative (always true for u64).
        Ok(())
    }

    fn validate(&self) -> Result<(), PluginError> {
        if self.min_interval_ms == 0 {
            return Err(PluginError {
                plugin: self.id().to_string(),
                message: "min_interval_ms must be > 0; set to 0 only disables filtering completely (use a no-op instead)".to_string(),
            });
        }
        Ok(())
    }
}

impl Default for AdmissionPlugin {
    fn default() -> Self {
        Self::new(60_000) // 1 minute
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::Once;
    use crate::schema::{Goal, Outcome, Trigger};

    /// Helper: create a Tick SpanContext for a Persona.
    fn tick_span(persona: &str, seq: u64, objective: &str) -> SpanContext {
        SpanContext {
            persona: PersonaId(persona.to_string()),
            goal: Goal {
                id: format!("tick-{seq}"),
                revision: 0,
                objective: objective.to_string(),
                trigger: Trigger::Tick { sequence: seq },
            },
        }
    }

    /// Helper: create an Utterance SpanContext.
    fn utterance_span(persona: &str, content: &str) -> SpanContext {
        SpanContext {
            persona: PersonaId(persona.to_string()),
            goal: Goal {
                id: "utter".to_string(),
                revision: 0,
                objective: "respond".to_string(),
                trigger: Trigger::Utterance {
                    from: "user".to_string(),
                    content: content.to_string(),
                },
            },
        }
    }

    /// Helper: create a Signal SpanContext.
    fn signal_span(persona: &str, name: &str, value: f64) -> SpanContext {
        SpanContext {
            persona: PersonaId(persona.to_string()),
            goal: Goal {
                id: "sig".to_string(),
                revision: 0,
                objective: "react".to_string(),
                trigger: Trigger::Signal {
                    name: name.to_string(),
                    value,
                },
            },
        }
    }

    /// Helper: create an Event SpanContext.
    fn event_span(persona: &str, topic: &str) -> SpanContext {
        SpanContext {
            persona: PersonaId(persona.to_string()),
            goal: Goal {
                id: "evt".to_string(),
                revision: 0,
                objective: "handle".to_string(),
                trigger: Trigger::Event {
                    topic: topic.to_string(),
                    payload: serde_json::json!({}),
                },
            },
        }
    }

    // ------------------------------------------------------------------
    // Admission: basic tick filtering
    // ------------------------------------------------------------------

    #[test]
    fn first_tick_is_always_admitted() {
        let inner = Once(Some(tick_span("alice", 1, "heartbeat")));
        let mut filter = AdmissionFilter::new(inner, 60_000);

        let span = filter.next_goal();
        assert!(span.is_some(), "first tick should be admitted");
        assert_eq!(span.unwrap().persona.as_str(), "alice");
    }

    #[test]
    fn consecutive_identical_ticks_are_dropped() {
        // Use a custom inner that yields two ticks through the same filter.
        use std::sync::atomic::{AtomicU8, Ordering};
        static TC_CALLS: AtomicU8 = AtomicU8::new(0);
        struct TwoTicks;
        impl Observations for TwoTicks {
            fn next_goal(&mut self) -> Option<SpanContext> {
                let n = TC_CALLS.fetch_add(1, Ordering::SeqCst);
                match n {
                    0 => Some(tick_span("alice", 1, "heartbeat")),
                    1 => Some(tick_span("alice", 2, "heartbeat")),
                    _ => None,
                }
            }
        }

        let mut filter = AdmissionFilter::new(TwoTicks, 60_000);
        let s1 = filter.next_goal();
        assert!(s1.is_some(), "first tick admitted");
        assert_eq!(s1.unwrap().goal.objective, "heartbeat");

        // Second identical tick: dropped because interval not elapsed
        // AND state unchanged.
        let s2 = filter.next_goal();
        assert!(s2.is_none(), "second identical tick must be dropped");
    }

    #[test]
    fn third_tick_with_changed_objective_is_admitted() {
        use std::sync::atomic::{AtomicU8, Ordering};
        static THREE_CALLS: AtomicU8 = AtomicU8::new(0);
        struct ThreeTicks;
        impl Observations for ThreeTicks {
            fn next_goal(&mut self) -> Option<SpanContext> {
                let n = THREE_CALLS.fetch_add(1, Ordering::SeqCst);
                match n {
                    0 => Some(tick_span("alice", 1, "heartbeat")),
                    1 => Some(tick_span("alice", 2, "heartbeat")),
                    2 => Some(tick_span("alice", 3, "URGENT: service down")),
                    _ => None,
                }
            }
        }

        let mut filter = AdmissionFilter::new(ThreeTicks, 60_000);
        // 1st tick admitted
        assert!(filter.next_goal().is_some());
        // 2nd identical tick dropped
        assert!(filter.next_goal().is_none());
        // 3rd tick has different objective → state changed → admitted
        let s3 = filter.next_goal();
        assert!(s3.is_some(), "changed objective should be admitted");
        assert_eq!(s3.unwrap().goal.objective, "URGENT: service down");
        assert_eq!(filter.tracked_personas(), 1);
    }

    // ------------------------------------------------------------------
    // Non-Tick triggers always admitted
    // ------------------------------------------------------------------

    #[test]
    fn utterance_always_admitted() {
        let inner = Once(Some(utterance_span("alice", "hello")));
        let mut filter = AdmissionFilter::new(inner, 60_000);
        assert!(filter.next_goal().is_some());
    }

    #[test]
    fn signal_always_admitted() {
        let inner = Once(Some(signal_span("bob", "temp", 91.0)));
        let mut filter = AdmissionFilter::new(inner, 60_000);
        assert!(filter.next_goal().is_some());
    }

    #[test]
    fn event_always_admitted() {
        let inner = Once(Some(event_span("bob", "webhook.received")));
        let mut filter = AdmissionFilter::new(inner, 60_000);
        assert!(filter.next_goal().is_some());
    }

    // ------------------------------------------------------------------
    // Persona isolation
    // ------------------------------------------------------------------

    #[test]
    fn different_personas_do_not_interfere() {
        use std::sync::atomic::{AtomicU8, Ordering};
        static MP_CALLS: AtomicU8 = AtomicU8::new(0);
        struct MultiPersona;
        impl Observations for MultiPersona {
            fn next_goal(&mut self) -> Option<SpanContext> {
                let n = MP_CALLS.fetch_add(1, Ordering::SeqCst);
                match n {
                    0 => Some(tick_span("alice", 1, "heartbeat")),
                    1 => Some(tick_span("bob", 1, "heartbeat")),    // different Persona
                    2 => Some(tick_span("alice", 2, "heartbeat")),  // same state, same persona → drop
                    3 => Some(tick_span("bob", 2, "heartbeat")),    // same state, same persona → drop
                    _ => None,
                }
            }
        }

        let mut filter = AdmissionFilter::new(MultiPersona, 60_000);
        assert_eq!(filter.next_goal().unwrap().persona.as_str(), "alice", "1st: alice admitted");
        assert_eq!(filter.next_goal().unwrap().persona.as_str(), "bob", "2nd: bob admitted (different persona)");
        assert!(filter.next_goal().is_none(), "3rd: alice's identical tick dropped");
        assert!(filter.next_goal().is_none(), "4th: bob's identical tick dropped");
        assert_eq!(filter.tracked_personas(), 2);
    }

    // ------------------------------------------------------------------
    // Empty / missing Persona binding
    // ------------------------------------------------------------------

    #[test]
    fn tick_without_persona_id_is_rejected() {
        let inner = Once(Some(tick_span("", 1, "heartbeat")));
        let mut filter = AdmissionFilter::new(inner, 60_000);
        assert!(filter.next_goal().is_none(),
            "tick with empty persona must be rejected");
    }

    #[test]
    fn tick_with_none_persona_is_rejected() {
        let inner = Once(Some(tick_span("none", 1, "heartbeat")));
        let mut filter = AdmissionFilter::new(inner, 60_000);
        assert!(filter.next_goal().is_none(),
            "tick with 'none' persona must be rejected");
    }

    // ------------------------------------------------------------------
    // AdmissionPlugin lifecycle
    // ------------------------------------------------------------------

    #[test]
    fn plugin_default_id_and_interval() {
        let p = AdmissionPlugin::default();
        assert_eq!(p.id(), "obs.admission");
        assert_eq!(p.min_interval_ms, 60_000);
    }

    #[test]
    fn plugin_provision_succeeds() {
        let mut p = AdmissionPlugin::new(30_000);
        assert!(p.provision().is_ok());
    }

    #[test]
    fn plugin_validate_rejects_zero_interval() {
        let p = AdmissionPlugin::new(0);
        let err = p.validate().unwrap_err();
        assert!(err.message.contains("min_interval_ms must be > 0"));
    }

    #[test]
    fn plugin_validate_accepts_reasonable_interval() {
        let p = AdmissionPlugin::new(60_000);
        assert!(p.validate().is_ok());
    }

    // ------------------------------------------------------------------
    // Integration: AdmissionFilter wrapping Observations in a loop span
    // ------------------------------------------------------------------

    #[test]
    fn admission_filter_works_with_loop_span() {
        use crate::events::{EventKind, EventStream, MemorySink};
        use crate::loop_engine::{AdmitAll, Loop};
        use crate::pipeline::{AllowAll, EchoExecutor, Pipeline};
        use crate::registry::CapabilityRegistry;
        use crate::schema::{ActionIntent, Capability, Context, Decision, Outcome, Provider, Value};

        // A provider that always expresses + concludes.
        struct PingProvider;
        impl Provider for PingProvider {
            fn id(&self) -> &str { "provider.ping" }
            fn decide(&self, _g: &Goal, _c: &Context, _caps: &[Capability]) -> Decision {
                Decision { intents: vec![
                    ActionIntent::Express { body: "pong".into() },
                    ActionIntent::Conclude { outcome: Outcome::Achieved },
                ]}
            }
        }

        // A source that yields two ticks, then a signal (always admitted).
        use std::sync::atomic::{AtomicU8, Ordering};
        static MIX_CALLS: AtomicU8 = AtomicU8::new(0);
        struct MixedSource;
        impl Observations for MixedSource {
            fn next_goal(&mut self) -> Option<SpanContext> {
                let n = MIX_CALLS.fetch_add(1, Ordering::SeqCst);
                match n {
                    0 => Some(tick_span("alice", 1, "ping")),
                    1 => Some(tick_span("alice", 2, "ping")),   // identical → dropped
                    2 => Some(signal_span("alice", "critical", 99.0)), // always admitted
                    _ => None,
                }
            }
        }

        let sink = MemorySink::new();
        let events_handle = sink.handle();
        let (stream, guard) = EventStream::spawn(sink);

        let mut reg = CapabilityRegistry::new();
        reg.register(Capability {
            id: "stub".into(), summary: "".into(),
            args_schema: serde_json::json!({"type":"object"}),
        }).unwrap();

        let pipeline = Pipeline {
            registry: &reg,
            governor: &AllowAll,
            executor: &EchoExecutor,
            events: &stream,
        };

        let provider = PingProvider;
        let lp = Loop { provider: &provider, admitter: &AdmitAll, pipeline: &pipeline, events: &stream };

        // Wrap in admission filter — source yields 3 goals, only 2 should
        // reach the loop.
        let mut filter = AdmissionFilter::new(MixedSource, 60_000);
        let report = lp.run_span(&mut filter, &Context::default());

        // The loop should have run twice: first tick admitted, second tick
        // dropped silently, then signal admitted.
        assert_eq!(report.expressed.len(), 2,
            "expected 2 expressed messages (tick + signal), not {}",
            report.expressed.len());

        stream.shutdown(guard);

        // Verify the event stream: the RunStarted events tell us which
        // goals actually entered the loop.
        let events = events_handle.lock().unwrap();
        let run_starts: Vec<_> = events.iter().filter_map(|e| match &e.kind {
            EventKind::RunStarted { goal_id, .. } => Some(goal_id.as_str()),
            _ => None,
        }).collect();
        // The order: tick seq=1 ("tick-1"), then signal ("sig")
        assert_eq!(run_starts, vec!["tick-1", "sig"],
            "only 2 spans should have started a run span");
    }
}
