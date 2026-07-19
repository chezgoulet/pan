//! # The loop — `observe → decide → enact → commit`, stream-driven.
//!
//! A "run" is a span over an observation stream. The discrete case (one chat
//! turn) is the degenerate single-observation span. Each step:
//!
//! - **observe**: take the latest [`Goal`] for the span. If a newer revision has
//!   arrived, it SUPERSEDES the one we were about to act on.
//! - **decide**: ask the [`Provider`] for a [`Decision`] (provider-agnostic).
//! - **enact**: route each intent. `Invoke` → the dispatch pipeline; `Express`
//!   → channels (here: an event); `Conclude` → terminate the span. **The
//!   abandon-path lives here**: before enacting a decision, re-check the goal;
//!   if it was superseded mid-decide, discard the whole decision unexecuted.
//! - **tool-use feedback (ReAct)**: if a decision *acts* (`Invoke`s a
//!   capability) without concluding, the executed results are folded back into
//!   the working context and the provider re-decides on the **same** goal — the
//!   agentic loop that lets a tool call inform the next step. A provider that
//!   neither acts nor concludes simply waits for the next external goal, and a
//!   runaway that never concludes is stopped at [`MAX_TOOL_STEPS`]. This is what
//!   makes an LLM (or any provider) able to *use* tools, not just name one.
//! - **commit**: collect outcomes/mutations and emit the run's record.
//!
//! The abandon mechanism is shared, by design, with the deferred §14 hardware
//! safety veto: both are "a decision in flight is dropped cleanly before its
//! effects reach the world." Building it once here means the veto path is a
//! matter of *who* sets the abandon flag, not new machinery.

use crate::events::{EventKind, EventStream};
use crate::pipeline::{Pipeline, PipelineError};
use crate::schema::{ActionIntent, Context, Decision, Fragment, Goal, Outcome, Provider, Scope};

/// The context channel on which the loop folds executed-capability results back
/// for the provider's next reasoning step (the ReAct feedback). It is opaque to
/// the core — a tool-using provider (e.g. an LLM) reads this channel to see what
/// its prior `Invoke`s produced; a rules/behavior-tree provider ignores it. Each
/// fragment body is a JSON object: `{"capability", "correlation"?, "result" | "error"}`.
pub const TOOL_RESULT_CHANNEL: &str = "tool_result";

/// Cap on agentic tool-use steps within a single goal, so a provider that keeps
/// invoking without ever concluding cannot loop forever. Reaching it ends the
/// span as [`RunEnd::StepLimit`] (a governor or a smarter budget can refine this
/// later; the core only guarantees the loop terminates).
const MAX_TOOL_STEPS: u32 = 8;

/// Why a run span ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    /// Provider concluded with this outcome.
    Concluded(Outcome),
    /// The goal was superseded before the decision could be enacted; the
    /// in-flight decision was discarded (abandon-path).
    Abandoned,
    /// The provider kept invoking capabilities without ever concluding and hit
    /// [`MAX_TOOL_STEPS`]; the span was stopped to guarantee termination.
    StepLimit,
    /// The observation stream ended without a conclusion.
    StreamExhausted,
}

/// The accumulated record of one run span.
#[derive(Debug, Default)]
pub struct RunReport {
    /// Effects that fully passed the pipeline and executed.
    pub effected: Vec<String>,
    /// The `(capability, result)` of each executed effect, in order — the same
    /// results the executor returned, surfaced synchronously so a caller (a
    /// channel, a UI) can display what a capability produced without racing the
    /// off-thread event stream.
    pub results: Vec<(String, crate::schema::Value)>,
    /// Effects that were attempted but failed/denied at some stage.
    pub failed: Vec<String>,
    /// Content emitted to channels.
    pub expressed: Vec<String>,
    /// How the span ended.
    pub end: Option<RunEnd>,
}

/// Source of goals for a run span. A discrete deployment yields exactly one; a
/// streaming/voice deployment yields evolving revisions of the same goal id.
/// This is the seam the manifest's "admission ↔ loop handoff for streaming"
/// open question plugs into — the loop only requires "give me the next goal, or
/// None when the span is done."
#[async_trait::async_trait]
pub trait Observations: Send {
    /// Return the next goal for this span, or `None` when the stream is done.
    async fn next_goal(&mut self) -> Option<Goal>;

    /// Resolve **when** a goal strictly newer than `current` becomes available,
    /// yielding it. This is a *future that fires on supersession*, not a poll: the
    /// loop races it against the provider's `decide`, so when it resolves the
    /// in-flight decision is dropped mid-flight (the abandon-path).
    ///
    /// The default never resolves — a discrete source has no supersession, so the
    /// future stays pending forever and the loop always takes the decide branch.
    /// A streaming/voice source overrides this to fire when a new revision lands.
    async fn superseded(&mut self, _current: &Goal) -> Goal {
        std::future::pending().await
    }
}

/// A single discrete goal — the degenerate one-observation span.
pub struct Once(pub Option<Goal>);
#[async_trait::async_trait]
impl Observations for Once {
    async fn next_goal(&mut self) -> Option<Goal> {
        self.0.take()
    }
}

/// The loop driver. Borrows the provider, the assembled context, and the wired
/// pipeline; runs spans against an [`Observations`] source.
pub struct Loop<'a> {
    pub provider: &'a dyn Provider,
    pub pipeline: &'a Pipeline<'a>,
    pub events: &'a EventStream,
    /// The authority under which this span's effects are dispatched — the
    /// persona (or subsystem) driving the loop. Every `Invoke` this span enacts
    /// is stamped with it, so the `govern` stage sees a consistent origin. A
    /// skill invoked mid-span narrows this further via its own
    /// [`ScopedInvoker`](crate::invoker::ScopedInvoker).
    pub scope: Scope,
}

impl<'a> Loop<'a> {
    /// Run one span to completion against an observation source and a context.
    /// Context assembly (the context family) is upstream of this in the full
    /// system; Wave 0 takes it as a parameter.
    pub async fn run_span(&self, obs: &mut dyn Observations, ctx: &Context) -> RunReport {
        let mut report = RunReport::default();

        // observe: take the first/next goal for the span.
        let mut current = match obs.next_goal().await {
            Some(g) => g,
            None => {
                report.end = Some(RunEnd::StreamExhausted);
                return report;
            }
        };

        // OUTER: one iteration per distinct goal pursued in this span.
        'goal: loop {
            // A reasoning context private to this goal. Executed tool results are
            // folded in across ReAct steps so the provider sees what its prior
            // invokes produced; a new goal (or a superseding revision) starts
            // fresh. `ctx` is the assembled base; we never mutate the caller's.
            let mut working_ctx = ctx.clone();
            let mut tool_steps: u32 = 0;

            // INNER: ReAct — decide, act, fold results, decide again on the SAME
            // goal until the provider concludes (or the step cap trips).
            loop {
                self.events.emit(EventKind::RunStarted {
                    goal_id: current.id.clone(),
                    revision: current.revision,
                });

                let caps = self.pipeline.registry.all();

                // ABANDON-PATH (concurrent). Race the provider's `decide` against
                // goal supersession. If a newer revision arrives *mid-decide*, the
                // decide future is DROPPED (cancelled) unexecuted and we re-decide
                // on the new revision — the in-flight work never reaches enact.
                // `biased` polls supersession first, so a revision that is already
                // available preempts a fresh decide. This is the exact hook the
                // §14 hardware safety veto reuses: who sets the abandon signal
                // changes, the machinery does not.
                //
                // Both futures borrow a per-iteration `snapshot`, never `current`
                // itself, so the supersession arm can reassign `current` without
                // colliding with the (now-dropped) decide future's borrow. See ADR
                // 0001, D4.
                let snapshot = current.clone();
                let decision: Decision = tokio::select! {
                    biased;
                    newer = obs.superseded(&snapshot) => {
                        self.events.emit(EventKind::Abandoned {
                            goal_id: current.id.clone(),
                            superseded_by: newer.revision,
                        });
                        current = newer;
                        continue 'goal; // re-decide on the newer revision, fresh context; nothing enacted.
                    }
                    decision = self.provider.decide(&snapshot, &working_ctx, &caps) => decision,
                };
                self.events.emit(EventKind::Decided {
                    provider: self.provider.id().to_string(),
                    intents: decision.intents.len(),
                });

                // enact: route each intent. A terminal outcome ends the span; any
                // executed effects come back as fragments to feed the next step.
                let (outcome, tool_results) = self.enact(&decision, &mut report).await;

                // A terminal conclusion ends the span, whatever else was enacted.
                if let Some(o @ (Outcome::Achieved | Outcome::Abandoned)) = outcome {
                    self.events.emit(EventKind::RunConcluded {
                        goal_id: current.id.clone(),
                        outcome: o,
                    });
                    report.end = Some(RunEnd::Concluded(o));
                    return report;
                }

                // Not concluded. If the provider ACTED, feed the results back and
                // let it reason again on the same goal (the agentic tool-use step),
                // bounded so it cannot loop forever.
                if !tool_results.is_empty() {
                    tool_steps += 1;
                    if tool_steps >= MAX_TOOL_STEPS {
                        self.events.emit(EventKind::RunConcluded {
                            goal_id: current.id.clone(),
                            outcome: Outcome::Abandoned,
                        });
                        report.end = Some(RunEnd::StepLimit);
                        return report;
                    }
                    working_ctx.fragments.extend(tool_results);
                    continue; // re-decide on the same goal, results in hand.
                }

                // Neither concluded nor acted: a genuine Continue. Step to the next
                // external goal if the stream has one, else the span is exhausted.
                match obs.next_goal().await {
                    Some(g) => {
                        current = g;
                        continue 'goal;
                    }
                    None => {
                        report.end = Some(RunEnd::StreamExhausted);
                        return report;
                    }
                }
            }
        }
    }

    /// enact one decision: dispatch effects, emit expressions, surface the
    /// terminal outcome. Returns `(concluding outcome if any, tool-result
    /// fragments)`. Each `Invoke` — whether it succeeded or was denied/failed —
    /// yields one fragment on the [`TOOL_RESULT_CHANNEL`], so the ReAct step can
    /// let the provider react to *both* results and errors. A tool error is
    /// information to an agent, not a fatal event.
    async fn enact(
        &self,
        decision: &Decision,
        report: &mut RunReport,
    ) -> (Option<Outcome>, Vec<Fragment>) {
        let mut outcome = None;
        let mut tool_results: Vec<Fragment> = Vec::new();
        for intent in &decision.intents {
            match intent {
                ActionIntent::Invoke {
                    capability,
                    args,
                    correlation,
                } => {
                    let req = crate::pipeline::EffectRequest {
                        capability: capability.clone(),
                        args: args.clone(),
                        correlation: correlation.clone(),
                        scope: self.scope.clone(),
                    };
                    match self.pipeline.dispatch(req).await {
                        Ok(eff) => {
                            report.effected.push(eff.capability.clone());
                            tool_results.push(tool_result_fragment(
                                &eff.capability,
                                correlation.as_deref(),
                                Ok(&eff.result),
                            ));
                            report.results.push((eff.capability, eff.result));
                        }
                        Err(e) => {
                            report.failed.push(capability.clone());
                            let message = pipeline_error_message(&e);
                            tool_results.push(tool_result_fragment(
                                capability,
                                correlation.as_deref(),
                                Err(&message),
                            ));
                            self.events.emit(EventKind::PluginError {
                                plugin: capability.to_string(),
                                message,
                            });
                        }
                    }
                }
                ActionIntent::Express { body } => {
                    self.events
                        .emit(EventKind::Expressed { body: body.clone() });
                    report.expressed.push(body.clone());
                }
                ActionIntent::Conclude { outcome: o } => {
                    outcome = Some(*o);
                }
            }
        }
        (outcome, tool_results)
    }
}

/// A one-line, human-and-provider-readable summary of why an effect did not
/// execute. Used both for the `PluginError` event and for the tool-result
/// fragment fed back to the provider.
fn pipeline_error_message(err: &PipelineError) -> String {
    match err {
        PipelineError::Unresolved { .. } => "capability not registered".to_string(),
        PipelineError::Invalid { reason, .. } => format!("invalid args: {reason}"),
        PipelineError::Rejected(r) => format!("governance rejected: {:?}", r.verdict),
        PipelineError::Execution { reason, .. } => format!("execution failed: {reason}"),
    }
}

/// Build the fragment the loop folds back into the working context after an
/// `Invoke`, on the [`TOOL_RESULT_CHANNEL`]. The body is a compact JSON object a
/// provider can parse to match the result to the call it made (`correlation`) —
/// or ignore entirely.
fn tool_result_fragment(
    capability: &str,
    correlation: Option<&str>,
    result: Result<&crate::schema::Value, &str>,
) -> Fragment {
    let mut body = serde_json::Map::new();
    body.insert("capability".into(), capability.into());
    if let Some(c) = correlation {
        body.insert("correlation".into(), c.into());
    }
    match result {
        Ok(value) => {
            body.insert("result".into(), value.clone());
        }
        Err(message) => {
            body.insert("error".into(), message.into());
        }
    }
    Fragment {
        channel: TOOL_RESULT_CHANNEL.to_string(),
        body: crate::schema::Value::Object(body).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventStream, MemorySink};
    use crate::pipeline::{AllowAll, EchoExecutor, Pipeline};
    use crate::registry::CapabilityRegistry;
    use crate::schema::{Capability, Trigger, Value};

    fn cap(id: &str) -> Capability {
        Capability {
            id: id.into(),
            summary: "".into(),
            args_schema: serde_json::json!({"type":"object"}),
        }
    }
    fn goal(id: &str, rev: u64) -> Goal {
        Goal {
            id: id.into(),
            revision: rev,
            objective: "o".into(),
            trigger: Trigger::Tick { sequence: 0 },
        }
    }

    /// A provider that emits a fixed decision, optionally returning different
    /// decisions per revision to test the abandon-path.
    struct ScriptedProvider {
        decision: Decision,
    }
    #[async_trait::async_trait]
    impl Provider for ScriptedProvider {
        fn id(&self) -> &str {
            "provider.scripted"
        }
        async fn decide(&self, _g: &Goal, _c: &Context, _caps: &[Capability]) -> Decision {
            self.decision.clone()
        }
    }

    #[tokio::test]
    async fn run_span_executes_invoke_and_concludes() {
        let mut reg = CapabilityRegistry::new();
        reg.register(cap("alert.raise")).unwrap();
        let mut stream = EventStream::spawn(MemorySink::new());
        let pipe = Pipeline {
            registry: &reg,
            governor: &AllowAll,
            executor: &EchoExecutor,
            events: &stream,
        };
        let provider = ScriptedProvider {
            decision: Decision {
                intents: vec![
                    ActionIntent::Express {
                        body: "working".into(),
                    },
                    ActionIntent::Invoke {
                        capability: "alert.raise".into(),
                        args: serde_json::json!({"level":"high"}),
                        correlation: None,
                    },
                    ActionIntent::Conclude {
                        outcome: Outcome::Achieved,
                    },
                ],
            },
        };
        let lp = Loop {
            provider: &provider,
            pipeline: &pipe,
            events: &stream,
            scope: Scope::system(),
        };
        let mut obs = Once(Some(goal("g1", 0)));
        let report = lp.run_span(&mut obs, &Context::default()).await;
        assert_eq!(report.effected, vec!["alert.raise"]);
        assert_eq!(report.expressed, vec!["working"]);
        assert_eq!(report.end, Some(RunEnd::Concluded(Outcome::Achieved)));
        stream.shutdown();
    }

    /// An observation source that hands out g@0, then reports a superseding g@1
    /// exactly once, to drive the abandon-path. `superseded` resolves immediately
    /// the first time (a newer revision is already waiting) and never again.
    struct Superseding {
        first: Option<Goal>,
        newer: Option<Goal>,
    }
    #[async_trait::async_trait]
    impl Observations for Superseding {
        async fn next_goal(&mut self) -> Option<Goal> {
            self.first.take()
        }
        async fn superseded(&mut self, current: &Goal) -> Goal {
            match self.newer.take() {
                Some(n) if current.superseded_by(&n) => n,
                _ => std::future::pending().await,
            }
        }
    }

    #[tokio::test]
    async fn superseded_decision_is_abandoned_not_executed() {
        let mut reg = CapabilityRegistry::new();
        reg.register(cap("danger.fire")).unwrap();
        let mut stream = EventStream::spawn(MemorySink::new());
        let pipe = Pipeline {
            registry: &reg,
            governor: &AllowAll,
            executor: &EchoExecutor,
            events: &stream,
        };
        // The provider always wants to fire the effect; the abandon-path must
        // prevent it on the superseded revision, then conclude on the new one.
        let provider = ScriptedProvider {
            decision: Decision {
                intents: vec![
                    ActionIntent::Invoke {
                        capability: "danger.fire".into(),
                        args: serde_json::json!({}),
                        correlation: None,
                    },
                    ActionIntent::Conclude {
                        outcome: Outcome::Achieved,
                    },
                ],
            },
        };
        let lp = Loop {
            provider: &provider,
            pipeline: &pipe,
            events: &stream,
            scope: Scope::system(),
        };
        let mut obs = Superseding {
            first: Some(goal("g", 0)),
            newer: Some(goal("g", 1)),
        };
        let report = lp.run_span(&mut obs, &Context::default()).await;
        // First revision's decision was discarded; only the re-decide on rev 1
        // actually executed the effect once.
        assert_eq!(
            report.effected,
            vec!["danger.fire"],
            "effect should fire exactly once, on the surviving revision"
        );
        assert_eq!(report.end, Some(RunEnd::Concluded(Outcome::Achieved)));
        stream.shutdown();
    }

    #[tokio::test]
    async fn failed_effect_is_recorded_not_fatal() {
        let reg = CapabilityRegistry::new(); // empty → resolve fails
        let mut stream = EventStream::spawn(MemorySink::new());
        let pipe = Pipeline {
            registry: &reg,
            governor: &AllowAll,
            executor: &EchoExecutor,
            events: &stream,
        };
        let provider = ScriptedProvider {
            decision: Decision {
                intents: vec![
                    ActionIntent::Invoke {
                        capability: "ghost".into(),
                        args: Value::Null,
                        correlation: None,
                    },
                    ActionIntent::Conclude {
                        outcome: Outcome::Achieved,
                    },
                ],
            },
        };
        let lp = Loop {
            provider: &provider,
            pipeline: &pipe,
            events: &stream,
            scope: Scope::system(),
        };
        let mut obs = Once(Some(goal("g", 0)));
        let report = lp.run_span(&mut obs, &Context::default()).await;
        assert_eq!(report.failed, vec!["ghost"]);
        assert!(report.effected.is_empty());
        assert_eq!(report.end, Some(RunEnd::Concluded(Outcome::Achieved)));
        stream.shutdown();
    }

    // --- D4: the abandon-path actually CANCELS an in-flight decide ------------

    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// A provider whose `decide` takes real time and records *completion*. If the
    /// future is cancelled mid-flight, the completion counter never increments —
    /// which is exactly how we observe that the abandon-path dropped it.
    struct SlowProvider {
        completed: Arc<AtomicU64>,
        decision: Decision,
        delay: Duration,
    }
    #[async_trait::async_trait]
    impl Provider for SlowProvider {
        fn id(&self) -> &str {
            "provider.slow"
        }
        async fn decide(&self, _g: &Goal, _c: &Context, _caps: &[Capability]) -> Decision {
            tokio::time::sleep(self.delay).await;
            // Only reached if the future was NOT cancelled before this point.
            self.completed.fetch_add(1, Ordering::SeqCst);
            self.decision.clone()
        }
    }

    /// Hands out g@0, then fires supersession to g@1 after a delay — long enough
    /// to land *while* the slow provider is mid-decide, short enough to preempt it.
    struct SupersedeAfter {
        first: Option<Goal>,
        newer: Option<Goal>,
        after: Duration,
    }
    #[async_trait::async_trait]
    impl Observations for SupersedeAfter {
        async fn next_goal(&mut self) -> Option<Goal> {
            self.first.take()
        }
        async fn superseded(&mut self, current: &Goal) -> Goal {
            match self.newer.take() {
                Some(n) if current.superseded_by(&n) => {
                    tokio::time::sleep(self.after).await;
                    n
                }
                _ => std::future::pending().await,
            }
        }
    }

    /// THE D4 GUARANTEE: a supersession arriving mid-decide cancels the in-flight
    /// decide future *before it completes*. If the abandon were merely a post-hoc
    /// check (wait for decide, then notice supersession), the slow provider would
    /// complete twice (rev 0 and rev 1). Because it is a true concurrent cancel,
    /// only the surviving revision's decide runs to completion.
    #[tokio::test]
    async fn supersession_mid_decide_cancels_the_decide_future() {
        let mut reg = CapabilityRegistry::new();
        reg.register(cap("danger.fire")).unwrap();
        let mut stream = EventStream::spawn(MemorySink::new());
        let pipe = Pipeline {
            registry: &reg,
            governor: &AllowAll,
            executor: &EchoExecutor,
            events: &stream,
        };
        let completed = Arc::new(AtomicU64::new(0));
        let provider = SlowProvider {
            completed: Arc::clone(&completed),
            delay: Duration::from_millis(120),
            decision: Decision {
                intents: vec![
                    ActionIntent::Invoke {
                        capability: "danger.fire".into(),
                        args: serde_json::json!({}),
                        correlation: None,
                    },
                    ActionIntent::Conclude {
                        outcome: Outcome::Achieved,
                    },
                ],
            },
        };
        let lp = Loop {
            provider: &provider,
            pipeline: &pipe,
            events: &stream,
            scope: Scope::system(),
        };
        let mut obs = SupersedeAfter {
            first: Some(goal("g", 0)),
            newer: Some(goal("g", 1)),
            after: Duration::from_millis(20),
        };
        let report = lp.run_span(&mut obs, &Context::default()).await;

        assert_eq!(
            completed.load(Ordering::SeqCst),
            1,
            "only the surviving revision's decide should complete; rev 0 was cancelled mid-flight"
        );
        assert_eq!(
            report.effected,
            vec!["danger.fire"],
            "the effect fires exactly once, on the surviving revision"
        );
        assert_eq!(report.end, Some(RunEnd::Concluded(Outcome::Achieved)));
        stream.shutdown();
    }

    // --- the agentic tool-use (ReAct) loop ------------------------------------

    /// A provider that invokes a tool on its first look at a goal, then — once it
    /// sees that tool's result folded into the context — answers and concludes.
    /// It concludes only when it can see its own `correlation` in the fed-back
    /// fragment, which proves the loop threads results (and correlation) back.
    struct ReActProvider {
        calls: Arc<AtomicU64>,
    }
    #[async_trait::async_trait]
    impl Provider for ReActProvider {
        fn id(&self) -> &str {
            "provider.react"
        }
        async fn decide(&self, _g: &Goal, ctx: &Context, _caps: &[Capability]) -> Decision {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let saw_result = ctx.fragments.iter().any(|f| {
                f.channel == TOOL_RESULT_CHANNEL
                    && f.body.contains("corr-1")
                    && f.body.contains("42")
            });
            if saw_result {
                Decision {
                    intents: vec![
                        ActionIntent::Express {
                            body: "the answer is 42".into(),
                        },
                        ActionIntent::Conclude {
                            outcome: Outcome::Achieved,
                        },
                    ],
                }
            } else {
                // Act without concluding: the loop must feed the result back.
                Decision {
                    intents: vec![ActionIntent::Invoke {
                        capability: "compute".into(),
                        args: serde_json::json!({}),
                        correlation: Some("corr-1".into()),
                    }],
                }
            }
        }
    }

    /// An executor that returns a fixed structured result, so the ReAct provider
    /// has something to react to.
    struct FixedExecutor;
    #[async_trait::async_trait]
    impl crate::pipeline::Executor for FixedExecutor {
        fn id(&self) -> &str {
            "exec.fixed"
        }
        async fn execute(
            &self,
            _capability: &str,
            _args: &Value,
        ) -> Result<Value, crate::pipeline::ExecError> {
            Ok(serde_json::json!({ "value": 42 }))
        }
    }

    #[tokio::test]
    async fn tool_result_feeds_back_and_the_provider_concludes() {
        let mut reg = CapabilityRegistry::new();
        reg.register(cap("compute")).unwrap();
        let mut stream = EventStream::spawn(MemorySink::new());
        let pipe = Pipeline {
            registry: &reg,
            governor: &AllowAll,
            executor: &FixedExecutor,
            events: &stream,
        };
        let calls = Arc::new(AtomicU64::new(0));
        let provider = ReActProvider {
            calls: Arc::clone(&calls),
        };
        let lp = Loop {
            provider: &provider,
            pipeline: &pipe,
            events: &stream,
            scope: Scope::system(),
        };
        let mut obs = Once(Some(goal("g", 0)));
        let report = lp.run_span(&mut obs, &Context::default()).await;

        // Decided twice: once to invoke, once (having seen the result) to answer.
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(report.effected, vec!["compute"]);
        assert_eq!(report.expressed, vec!["the answer is 42"]);
        assert_eq!(report.end, Some(RunEnd::Concluded(Outcome::Achieved)));
        stream.shutdown();
    }

    /// A provider that always invokes and never concludes — the runaway the step
    /// cap exists to stop.
    struct RunawayProvider;
    #[async_trait::async_trait]
    impl Provider for RunawayProvider {
        fn id(&self) -> &str {
            "provider.runaway"
        }
        async fn decide(&self, _g: &Goal, _c: &Context, _caps: &[Capability]) -> Decision {
            Decision {
                intents: vec![ActionIntent::Invoke {
                    capability: "compute".into(),
                    args: serde_json::json!({}),
                    correlation: None,
                }],
            }
        }
    }

    #[tokio::test]
    async fn a_provider_that_never_concludes_hits_the_step_limit() {
        let mut reg = CapabilityRegistry::new();
        reg.register(cap("compute")).unwrap();
        let mut stream = EventStream::spawn(MemorySink::new());
        let pipe = Pipeline {
            registry: &reg,
            governor: &AllowAll,
            executor: &FixedExecutor,
            events: &stream,
        };
        let provider = RunawayProvider;
        let lp = Loop {
            provider: &provider,
            pipeline: &pipe,
            events: &stream,
            scope: Scope::system(),
        };
        let mut obs = Once(Some(goal("g", 0)));
        let report = lp.run_span(&mut obs, &Context::default()).await;

        assert_eq!(report.end, Some(RunEnd::StepLimit));
        assert_eq!(
            report.effected.len(),
            MAX_TOOL_STEPS as usize,
            "the effect fires once per step until the cap stops the span"
        );
        stream.shutdown();
    }
}
