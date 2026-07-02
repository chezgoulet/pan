//! # The daemon session — the per-connection state machine.
//!
//! One [`Session`] per TCP connection. The session owns:
//!
//! - The `seq` counter for outgoing messages (handshake `welcome` is seq 0;
//!   subsequent acks / decisions increment).
//! - The host's registered capability set (the [`CapabilityRegistry`]).
//! - The set of instantiated souls (soul_id -> [`SoulState`]). In M1, every
//!   instantiated soul is `mind: rules` and carries its rule list in the
//!   `soul` birth-state field; the session parses a `rules: [...]` array out
//!   of that opaque JSON.
//! - The [`pan_core::pipeline::Pipeline`] and event stream used to enact
//!   `Invoke` intents (resolve → validate → govern → execute → record).
//!
//! ## Lifecycle
//!
//! 1. **Handshake**: receive `hello`, check version + profile, send `welcome`.
//! 2. **`register_capabilities`**: replace the registry with the host's set;
//!    send `ack`.
//! 3. **`instantiate_soul`**: store the soul + mind + (for `rules`) the rule
//!    list; send `ack`.
//! 4. **`perceive`** (the steady state): for the named soul, run the rules
//!    provider against the (goal, context). The result is a `Decision`. Any
//!    `ActionIntent::Invoke` runs through the dispatch pipeline. If any
//!    Invoke fails, the wire reply is an `error` message (carrying the
//!    matching code from the closed set: `unknown_capability`,
//!    `invalid_args`, `provider_failure`); if every Invoke succeeds, the
//!    reply is a `decision` body whose `intents` are the provider's intents
//!    unchanged. (The Soul Protocol: the daemon's `validate` stage replies
//!    `error: unknown_capability` — see conformance fixture 09.)
//! 5. **`release_soul`**: drop the soul's state; send `ack`.
//! 6. **`shutdown`**: send `ack`; the server then closes the connection.
//!
//! `seq` and `re` are tracked here so call sites don't have to.

use std::collections::HashMap;

use pan_core::pipeline::{AllowAll, EchoExecutor, Pipeline, PipelineError};
use pan_core::providers::rules::{Rule, RulesProvider};
use pan_core::registry::CapabilityRegistry;
use pan_core::schema::{
    self as v, ActionIntent, Capability, Context, Decision, Goal, Provider,
};

use crate::governor::ResolveGovernor;
use crate::wire::{
    AckBody, Body, DecisionBody, Envelope, ErrorBody, HelloBody, InstantiateSoulBody,
    MessageType, MindKind, PerceiveBody, PROTOCOL_VERSION, RegisterCapabilitiesBody,
    SERVER_IDENTITY, WelcomeBody,
};

/// The "minds" this daemon advertises in `welcome.minds`. M1 advertises rules
/// only; llm/behavior_tree are reserved for future sprints.
fn advertised_minds() -> Vec<MindKind> { vec![MindKind::Rules] }

/// Per-soul runtime state. For `mind: rules`, the rule list parsed from the
/// soul birth-state. For other minds (M1 stub), no rules — the session falls
/// through to a "no rule fired" decision.
struct SoulState {
    #[allow(dead_code)]
    mind: MindKind,
    #[allow(dead_code)]
    soul: serde_json::Value,
    rules: Vec<Rule>,
}

impl SoulState {
    /// Parse a `SoulState` from an `instantiate_soul` body. For `rules`-minded
    /// souls we read `soul.rules: [{when_event_topic, then_invoke}, ...]` out
    /// of the opaque birth-state. Other minds are admitted but unused at M1.
    fn from_body(body: &InstantiateSoulBody) -> Self {
        let rules = if body.mind == MindKind::Rules {
            parse_rules_from_soul(&body.soul)
        } else {
            Vec::new()
        };
        SoulState { mind: body.mind, soul: body.soul.clone(), rules }
    }

    /// Build a rules provider for this soul. For non-rules minds (which at
    /// M1 have no provider), return `None` — the session emits a Continue-only
    /// decision in that case.
    fn provider(&self) -> Option<RulesProvider> {
        match self.mind {
            MindKind::Rules => Some(RulesProvider { rules: self.rules.clone() }),
            // Future: behavior tree / LLM providers live here.
            _ => None,
        }
    }
}

/// Pull a list of `Rule` out of the opaque `soul` JSON. The expected shape is:
///
/// ```json
/// { "rules": [
///     { "when_event_topic": "combat.crew_saved",
///       "then_invoke": { "capability": "npc.move_to", "args": { "room": "cockpit" } } },
///     ...
/// ] }
/// ```
///
/// This is the *soul file*'s rules field — the same kind of content the
/// narrative team authors alongside soul birth-state. The daemon treats it as
/// data, not code (it can't `Invoke` arbitrary capabilities; every Invoke is
/// gated by the registered set).
fn parse_rules_from_soul(soul: &serde_json::Value) -> Vec<Rule> {
    let Some(arr) = soul.get("rules").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in arr {
        let when_event_topic = entry.get("when_event_topic")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        let when_signal_over = entry.get("when_signal_over").and_then(|s| {
            // Expect an object: {"name": "...", "threshold": 0.5}
            let name = s.get("name").and_then(|n| n.as_str())?.to_string();
            let threshold = s.get("threshold").and_then(|t| t.as_f64())?;
            Some((name, threshold))
        });
        let then_invoke = entry.get("then_invoke").and_then(|t| {
            let cap = t.get("capability").and_then(|c| c.as_str())?.to_string();
            let args = t.get("args").cloned().unwrap_or(serde_json::json!({}));
            Some((cap, args))
        });
        if let Some(then) = then_invoke {
            out.push(Rule {
                when_signal_over,
                when_event_topic,
                then_invoke: then,
            });
        }
    }
    out
}

/// The error / status line a session produces for a *wire-level* problem
/// (handshake failed, unknown soul, etc.) — distinct from a `PipelineError`
/// which is a *pipeline-level* problem surfaced from an `Invoke`.
#[derive(Debug)]
pub enum SessionError {
    /// A line failed to parse. The protocol says: reply with `error: bad_frame`,
    /// keep the connection open.
    BadFrame(String),
    /// An inbound `type` we don't know. Schema-violating; reply with
    /// `error: unknown_type`, keep the connection open.
    UnknownType(String),
    /// Version mismatch at handshake: reply with `error: version_unsupported`
    /// and close.
    VersionUnsupported { client: u32, ours: u32 },
    /// A `perceive` named a soul we don't know. Reply `error: unknown_soul`.
    UnknownSoul(String),
}

/// What `Session::dispatch_decision` returns. Either every Invoke passed (Ok
/// with the surviving intents — which, on the happy path, are the same
/// intents the provider emitted) or the FIRST failure (Failed, carrying the
/// wire error code + a human message).
enum DispatchOutcome {
    Ok { intents: Vec<ActionIntent> },
    Failed { code: crate::wire::ErrorCode, message: String },
}

/// Map a pipeline-level error to the wire's closed-set `ErrorCode`.
fn pipeline_err_to_wire(e: &PipelineError) -> crate::wire::ErrorCode {
    match e {
        PipelineError::Unresolved { .. } => crate::wire::ErrorCode::UnknownCapability,
        PipelineError::Invalid { .. }    => crate::wire::ErrorCode::InvalidArgs,
        PipelineError::Rejected(_)       => crate::wire::ErrorCode::ProviderFailure,
        PipelineError::Execution { .. }  => crate::wire::ErrorCode::ProviderFailure,
    }
}

/// Build the human message for an `error` reply. Includes the capability id
/// where relevant so a host log can identify the failing call.
fn pipeline_err_message(e: &PipelineError) -> String {
    match e {
        PipelineError::Unresolved { capability } =>
            format!("provider requested `{capability}` which was never registered"),
        PipelineError::Invalid { capability, reason } =>
            format!("invalid args for `{capability}`: {reason}"),
        PipelineError::Rejected(r) =>
            format!("governor rejected `{}`: {:?}", r.capability, r.verdict),
        PipelineError::Execution { capability, reason } =>
            format!("executor failed for `{capability}`: {reason}"),
    }
}

/// The session itself. The connection driver ([`crate::server`]) reads lines,
/// hands them to `Session::handle`, and writes the returned envelopes back
/// over the connection.
pub struct Session {
    next_seq: u64,
    registry: CapabilityRegistry,
    souls: HashMap<String, SoulState>,
}

impl Session {
    /// Construct a new session with an empty registry and no instantiated
    /// souls. The event stream (a per-call `DiscardSink`) is built fresh in
    /// `dispatch_decision` so the pipeline can emit — the session itself
    /// does not need to inspect events.
    pub fn new() -> Self {
        Session {
            next_seq: 0,
            registry: CapabilityRegistry::new(),
            souls: HashMap::new(),
        }
    }

    /// Allocate the next outgoing `seq` and bump the counter.
    fn next_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    /// Build an outgoing envelope with the session's `seq`. The session
    /// always sets `re` to the inbound `seq` for response messages.
    fn out(&mut self, re: Option<u64>, body: Body) -> Envelope {
        Envelope::outgoing(self.next_seq(), re, body)
    }

    /// Handle one inbound envelope. Returns a *vector* of outgoing envelopes
    /// (in order) — most calls return exactly one. The vector shape keeps
    /// future bulk responses (e.g. heartbeats) cheap to add.
    ///
    /// `SessionError` is reserved for cases where the session wants the
    /// driver to *close* the connection (currently: `VersionUnsupported`).
    /// All other errors are reported as `error` wire messages; the connection
    /// stays open.
    pub fn handle(&mut self, env: Envelope) -> Result<Vec<Envelope>, SessionError> {
        match env.ty {
            MessageType::Hello => self.on_hello(env.seq, env.body),
            MessageType::Welcome => {
                // Daemon does not receive `welcome`; it's daemon → host only.
                self.error_response(env.seq, crate::wire::ErrorCode::UnknownType, "welcome is daemon-to-host only")
            }
            MessageType::RegisterCapabilities => self.on_register(env.seq, env.body),
            MessageType::InstantiateSoul => self.on_instantiate(env.seq, env.body),
            MessageType::ReleaseSoul => self.on_release(env.seq, env.body),
            MessageType::Perceive => self.on_perceive(env.seq, env.body),
            MessageType::Decision | MessageType::Ack => {
                // Daemon does not receive these; host → daemon only.
                self.error_response(env.seq, crate::wire::ErrorCode::UnknownType, "decision/ack is daemon-to-host only")
            }
            MessageType::Error => {
                // Daemon does not receive `error`; it's daemon → host only.
                // We still record the inbound `error` so a future observability
                // layer can audit it; for M1 we reply with another `error`
                // indicating the misuse.
                self.error_response(env.seq, crate::wire::ErrorCode::UnknownType, "error is daemon-to-host only")
            }
            MessageType::Shutdown => self.on_shutdown(env.seq, env.body),
        }
    }

    /// Adapter: turn a "no such wire type inbound here" into a proper
    /// `error: unknown_type` wire message.
    fn error_response(&mut self, re: u64, code: crate::wire::ErrorCode, message: &str) -> Result<Vec<Envelope>, SessionError> {
        Ok(vec![self.out(Some(re), Body::Error(ErrorBody {
            code, message: message.to_string(),
        }))])
    }

    fn on_hello(&mut self, re: u64, body: Body) -> Result<Vec<Envelope>, SessionError> {
        let Body::Hello(HelloBody { protocol_version, profile, client: _ }) = body else {
            return self.error_response(re, crate::wire::ErrorCode::BadFrame, "hello body shape");
        };
        if protocol_version != PROTOCOL_VERSION || profile != "reachlock/0" {
            return Err(SessionError::VersionUnsupported {
                client: protocol_version, ours: PROTOCOL_VERSION,
            });
        }
        let welcome = WelcomeBody {
            protocol_version: PROTOCOL_VERSION,
            server: SERVER_IDENTITY.to_string(),
            minds: advertised_minds(),
        };
        Ok(vec![self.out(Some(re), Body::Welcome(welcome))])
    }

    fn on_register(&mut self, re: u64, body: Body) -> Result<Vec<Envelope>, SessionError> {
        let Body::RegisterCapabilities(RegisterCapabilitiesBody { capabilities }) = body else {
            return self.error_response(re, crate::wire::ErrorCode::BadFrame, "register_capabilities body shape");
        };
        // Replace the registry. A duplicate id within the host's set is a host
        // bug — return an error rather than silently overwriting, matching the
        // core's "no last-wins" stance.
        let mut new_reg = CapabilityRegistry::new();
        for c in capabilities {
            if let Err(e) = new_reg.register(c) {
                return Ok(vec![self.out(Some(re), Body::Error(ErrorBody {
                    code: crate::wire::ErrorCode::ProviderFailure,
                    message: format!("register_capabilities: {e}"),
                }))]);
            }
        }
        self.registry = new_reg;
        Ok(vec![self.out(Some(re), Body::Ack(AckBody::default()))])
    }

    fn on_instantiate(&mut self, re: u64, body: Body) -> Result<Vec<Envelope>, SessionError> {
        let Body::InstantiateSoul(b) = body else {
            return self.error_response(re, crate::wire::ErrorCode::BadFrame, "instantiate_soul body shape");
        };
        // For M1, only `rules` is fully exercised; the others are accepted but
        // their provider is `None` (the session emits a Continue-only decision
        // on perceive). The host is told via welcome.minds, so this is not
        // surprising.
        self.souls.insert(b.soul_id.clone(), SoulState::from_body(&b));
        Ok(vec![self.out(Some(re), Body::Ack(AckBody::default()))])
    }

    fn on_release(&mut self, re: u64, body: Body) -> Result<Vec<Envelope>, SessionError> {
        let Body::ReleaseSoul(b) = body else {
            return self.error_response(re, crate::wire::ErrorCode::BadFrame, "release_soul body shape");
        };
        if self.souls.remove(&b.soul_id).is_none() {
            return Ok(vec![self.out(Some(re), Body::Error(ErrorBody {
                code: crate::wire::ErrorCode::UnknownSoul,
                message: format!("soul_id `{}` is not instantiated", b.soul_id),
            }))]);
        }
        Ok(vec![self.out(Some(re), Body::Ack(AckBody::default()))])
    }

    fn on_perceive(&mut self, re: u64, body: Body) -> Result<Vec<Envelope>, SessionError> {
        let Body::Perceive(PerceiveBody { soul_id, goal, context }) = body else {
            return self.error_response(re, crate::wire::ErrorCode::BadFrame, "perceive body shape");
        };
        // First, look up the soul. Unknown → error.
        let Some(soul) = self.souls.get(&soul_id) else {
            return Ok(vec![self.out(Some(re), Body::Error(ErrorBody {
                code: crate::wire::ErrorCode::UnknownSoul,
                message: format!("soul_id `{soul_id}` is not instantiated"),
            }))]);
        };

        // Ask the provider for a decision. If the soul has no provider (a
        // non-rules mind at M1), emit a Continue-only decision so the host
        // sees the daemon acknowledged the perceive.
        let provider: Box<dyn Provider> = match soul.provider() {
            Some(rp) => Box::new(rp),
            None => Box::new(NoProvider),
        };
        let caps: Vec<Capability> = self.registry.all();
        let decision = provider.decide(&goal, &context, &caps);

        // Enact: run every Invoke through the dispatch pipeline. The pipeline
        // borrows from `self`; we build it once here.
        //
        // On the `govern` slot: the wire-level `unknown_capability` check is
        // already enforced at the pipeline's `resolve` stage (it returns
        // `PipelineError::Unresolved`). We catch that below and surface the
        // wire-level error code. The `ResolveGovernor` is a separate, explicit
        // governor slot that a future wave can swap for a real `gov.policy`
        // without changing the wire contract.
        let (stream, guard) = pan_core::events::EventStream::spawn(pan_core::events::DiscardSink);
        let pipeline = Pipeline {
            registry: &self.registry,
            governor: &AllowAll,
            executor: &EchoExecutor,
            events: &stream,
        };
        // Reference ResolveGovernor to confirm it compiles and is part of the
        // daemon's M1 vocabulary. The wire-level check below uses the
        // pipeline's own resolve stage, which is the structural choke point.
        let _g = ResolveGovernor { registry: &self.registry };
        let _ = _g;
        let outcome = self.dispatch_decision(&decision, &pipeline);
        // Drop the stream so the consumer thread can exit; events were
        // discarded by the sink.
        drop(stream);
        guard.join();

        match outcome {
            DispatchOutcome::Ok { intents } => {
                // The wire's `decision` response carries the *original* goal
                // id and revision so the host can correlate.
                let body = DecisionBody {
                    soul_id,
                    goal_id: goal.id,
                    goal_revision: goal.revision,
                    decision: Decision { intents },
                };
                Ok(vec![self.out(Some(re), Body::Decision(body))])
            }
            DispatchOutcome::Failed { code, message } => {
                // The wire's `error` reply (per the Soul Protocol): the
                // daemon's validate stage replies with `error code:
                // unknown_capability` etc. on a failed Invoke. The host
                // correlates by `re`.
                Ok(vec![self.out(Some(re), Body::Error(ErrorBody {
                    code, message,
                }))])
            }
        }
    }

    /// Walk every intent in the decision. Invokes go through the dispatch
    /// pipeline. ANY failure short-circuits the response: the wire reply is
    /// an `error` message (not a `decision`) carrying the matching code from
    /// the Soul Protocol's closed set:
    ///
    /// - `PipelineError::Unresolved`  ->  `error code: "unknown_capability"`
    /// - `PipelineError::Invalid`     ->  `error code: "invalid_args"`
    /// - `PipelineError::Rejected`    ->  `error code: "provider_failure"`
    /// - `PipelineError::Execution`   ->  `error code: "provider_failure"`
    ///
    /// If every Invoke succeeds, we return the provider's intents unchanged
    /// (Express and Conclude are not world-effects and pass through). If the
    /// provider's decision had no Conclude, we append `Continue` so the
    /// host's loop reads a well-formed outcome.
    fn dispatch_decision(&self, decision: &Decision, pipeline: &Pipeline) -> DispatchOutcome {
        let mut out = Vec::new();
        for intent in &decision.intents {
            match intent {
                ActionIntent::Invoke { capability, args, correlation } => {
                    let req = pan_core::pipeline::EffectRequest {
                        capability: capability.clone(),
                        args: args.clone(),
                        correlation: correlation.clone(),
                    };
                    if let Err(e) = pipeline.dispatch(req) {
                        return DispatchOutcome::Failed {
                            code: pipeline_err_to_wire(&e),
                            message: pipeline_err_message(&e),
                        };
                    }
                    out.push(intent.clone());
                }
                // Express / Conclude are not world-effects; pass through.
                _ => out.push(intent.clone()),
            }
        }
        // If the original decision had no Conclude, append a Continue so the
        // wire's decision body is well-formed (the host's loop reads
        // `decision.outcome()`).
        if !out.iter().any(|i| matches!(i, ActionIntent::Conclude { .. })) {
            out.push(ActionIntent::Conclude { outcome: v::Outcome::Continue });
        }
        DispatchOutcome::Ok { intents: out }
    }

    fn on_shutdown(&mut self, re: u64, _body: Body) -> Result<Vec<Envelope>, SessionError> {
        // The protocol says: `shutdown` causes connection close. We send
        // `ack` first so the host sees a clean end; the driver then closes.
        Ok(vec![self.out(Some(re), Body::Ack(AckBody::default()))])
    }
}

/// A no-op provider used when an `instantiate_soul` requested a mind kind
/// this daemon doesn't yet support. Its `decide` returns a Continue-only
/// decision so the host gets a well-formed `decision` reply.
struct NoProvider;
impl Provider for NoProvider {
    fn id(&self) -> &str { "provider.none" }
    fn decide(&self, _g: &Goal, _c: &Context, _caps: &[Capability]) -> Decision {
        Decision { intents: vec![ActionIntent::Conclude { outcome: v::Outcome::Continue }] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{
        Body, Envelope, HelloBody, InstantiateSoulBody, MessageType, MindKind,
        PerceiveBody, RegisterCapabilitiesBody,
    };
    use pan_core::schema as v;

    fn hello_envelope(seq: u64) -> Envelope {
        Envelope {
            v: 0, seq, re: None, ty: MessageType::Hello,
            body: Body::Hello(HelloBody {
                protocol_version: 0,
                profile: "reachlock/0".into(),
                client: "test-client".into(),
            }),
        }
    }

    #[test]
    fn handshake_emits_welcome() {
        let mut s = Session::new();
        let out = s.handle(hello_envelope(0)).unwrap();
        assert_eq!(out.len(), 1);
        let env = &out[0];
        assert_eq!(env.ty, MessageType::Welcome);
        assert_eq!(env.re, Some(0));
        if let Body::Welcome(w) = &env.body {
            assert_eq!(w.protocol_version, 0);
            assert_eq!(w.server, "pan-serve/0.1.0");
            assert_eq!(w.minds, vec![MindKind::Rules]);
        } else { panic!("expected welcome body"); }
    }

    #[test]
    fn version_mismatch_closes() {
        let mut s = Session::new();
        let env = Envelope {
            v: 0, seq: 0, re: None, ty: MessageType::Hello,
            body: Body::Hello(HelloBody {
                protocol_version: 1, // wrong
                profile: "reachlock/0".into(),
                client: "x".into(),
            }),
        };
        let err = s.handle(env).unwrap_err();
        assert!(matches!(err, SessionError::VersionUnsupported { .. }));
    }

    #[test]
    fn register_then_instantiate_then_perceive_event_fires_rule() {
        let mut s = Session::new();
        // hello
        s.handle(hello_envelope(0)).unwrap();
        // register capabilities (npc.move_to)
        let reg = Envelope {
            v: 0, seq: 1, re: None, ty: MessageType::RegisterCapabilities,
            body: Body::RegisterCapabilities(RegisterCapabilitiesBody {
                capabilities: vec![Capability {
                    id: "npc.move_to".into(),
                    summary: "walk to a room".into(),
                    args_schema: serde_json::json!({"type":"object","required":["room"]}),
                }],
            }),
        };
        let out = s.handle(reg).unwrap();
        assert_eq!(out[0].ty, MessageType::Ack);

        // instantiate a rules soul with one event rule
        let inst = Envelope {
            v: 0, seq: 2, re: None, ty: MessageType::InstantiateSoul,
            body: Body::InstantiateSoul(InstantiateSoulBody {
                soul_id: "example_pilot".into(),
                mind: MindKind::Rules,
                soul: serde_json::json!({
                    "rules": [
                        { "when_event_topic": "combat.crew_saved",
                          "then_invoke": { "capability": "npc.move_to", "args": {"room": "cockpit"} } }
                    ]
                }),
            }),
        };
        let out = s.handle(inst).unwrap();
        assert_eq!(out[0].ty, MessageType::Ack);

        // perceive with the matching event topic
        let perc = Envelope {
            v: 0, seq: 3, re: None, ty: MessageType::Perceive,
            body: Body::Perceive(PerceiveBody {
                soul_id: "example_pilot".into(),
                goal: Goal {
                    id: "amb_00007".into(), revision: 1,
                    objective: "react".into(),
                    trigger: v::Trigger::Event {
                        topic: "combat.crew_saved".into(),
                        payload: serde_json::json!({}),
                    },
                },
                context: v::Context::default(),
            }),
        };
        let out = s.handle(perc).unwrap();
        assert_eq!(out.len(), 1);
        let env = &out[0];
        assert_eq!(env.ty, MessageType::Decision);
        if let Body::Decision(d) = &env.body {
            // The first intent should be the Invoke fired by the rule.
            match &d.decision.intents[0] {
                ActionIntent::Invoke { capability, args, .. } => {
                    assert_eq!(capability, "npc.move_to");
                    assert_eq!(args, &serde_json::json!({"room": "cockpit"}));
                }
                other => panic!("expected Invoke, got {other:?}"),
            }
            // A Conclude is appended.
            assert!(d.decision.intents.iter().any(|i| matches!(
                i, ActionIntent::Conclude { .. }
            )));
        } else { panic!("expected Decision body"); }
    }

    /// The unknown-capability path (conformance case 09): when the provider
    /// asks for an Invoke of a capability the host never registered, the
    /// daemon replies with an `error` message whose `code` is
    /// `"unknown_capability"`. The reply is NOT a `decision` body with an
    /// inline error — it's a wire-level `error` so the host can route it
    /// without parsing the decision payload.
    #[test]
    fn unknown_capability_emits_wire_error_not_decision() {
        let mut s = Session::new();
        s.handle(hello_envelope(0)).unwrap();
        // Register only npc.move_to; the rule below tries npc.fly_ship.
        s.handle(Envelope {
            v: 0, seq: 1, re: None, ty: MessageType::RegisterCapabilities,
            body: Body::RegisterCapabilities(RegisterCapabilitiesBody {
                capabilities: vec![Capability {
                    id: "npc.move_to".into(),
                    summary: "".into(),
                    args_schema: serde_json::json!({"type":"object"}),
                }],
            }),
        }).unwrap();
        s.handle(Envelope {
            v: 0, seq: 2, re: None, ty: MessageType::InstantiateSoul,
            body: Body::InstantiateSoul(InstantiateSoulBody {
                soul_id: "example_pilot".into(),
                mind: MindKind::Rules,
                soul: serde_json::json!({
                    "rules": [
                        { "when_event_topic": "test",
                          "then_invoke": { "capability": "npc.fly_ship", "args": {} } }
                    ]
                }),
            }),
        }).unwrap();
        let out = s.handle(Envelope {
            v: 0, seq: 3, re: None, ty: MessageType::Perceive,
            body: Body::Perceive(PerceiveBody {
                soul_id: "example_pilot".into(),
                goal: Goal {
                    id: "g".into(), revision: 1, objective: "x".into(),
                    trigger: v::Trigger::Event { topic: "test".into(), payload: serde_json::json!({}) },
                },
                context: v::Context::default(),
            }),
        }).unwrap();
        let env = &out[0];
        assert_eq!(env.re, Some(3), "error must echo the perceive seq as re");
        match &env.body {
            Body::Error(e) => {
                assert_eq!(e.code, crate::wire::ErrorCode::UnknownCapability,
                    "expected code=unknown_capability, got {:?}", e.code);
                assert!(e.message.contains("npc.fly_ship"),
                    "message should name the unknown capability: {}", e.message);
            }
            other => panic!("expected wire error body, got {other:?}"),
        }
    }

    #[test]
    fn unknown_soul_emits_error_response() {
        let mut s = Session::new();
        s.handle(hello_envelope(0)).unwrap();
        let out = s.handle(Envelope {
            v: 0, seq: 1, re: None, ty: MessageType::Perceive,
            body: Body::Perceive(PerceiveBody {
                soul_id: "no_such_soul".into(),
                goal: Goal {
                    id: "g".into(), revision: 1, objective: "x".into(),
                    trigger: v::Trigger::Tick { sequence: 1 },
                },
                context: v::Context::default(),
            }),
        }).unwrap();
        assert_eq!(out[0].ty, MessageType::Error);
        if let Body::Error(e) = &out[0].body {
            assert_eq!(e.code, crate::wire::ErrorCode::UnknownSoul);
        } else { panic!("expected error body"); }
    }

    #[test]
    fn shutdown_emits_ack() {
        let mut s = Session::new();
        s.handle(hello_envelope(0)).unwrap();
        let out = s.handle(Envelope {
            v: 0, seq: 1, re: None, ty: MessageType::Shutdown,
            body: crate::wire::Body::Shutdown(crate::wire::ShutdownBody::default()),
        }).unwrap();
        assert_eq!(out[0].ty, MessageType::Ack);
    }

    #[test]
    fn parse_rules_handles_event_and_signal() {
        let soul = serde_json::json!({
            "rules": [
                { "when_event_topic": "a.b",
                  "then_invoke": { "capability": "x", "args": { "k": 1 } } },
                { "when_signal_over": { "name": "hull", "threshold": 0.3 },
                  "then_invoke": { "capability": "y", "args": {} } }
            ]
        });
        let rules = parse_rules_from_soul(&soul);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].when_event_topic.as_deref(), Some("a.b"));
        assert!(rules[0].when_signal_over.is_none());
        assert!(rules[1].when_event_topic.is_none());
        assert_eq!(rules[1].when_signal_over.as_ref().map(|(n, _)| n.as_str()), Some("hull"));
    }
}
