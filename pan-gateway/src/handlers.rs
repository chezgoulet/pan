//! # Route handlers for the Pan gateway.
//!
//! Implements the OpenAI-compatible `/v1/chat/completions` endpoint and
//! Pan-native `/v1/agents/:name/goals`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json, Response,
    },
    routing::{get, post},
    Router,
};
use futures::Stream;
use serde::{Deserialize, Serialize};

use pan_agent::AssembledAgent;
use pan_core::events::{EventStream, TracingSink};
use pan_core::loop_engine::{Loop, Once, RunEnd, RunReport};
use pan_core::pipeline::Pipeline;
use pan_core::schema::{Context, Goal, Outcome, Scope, Trigger, Value};

use crate::pool::AgentPool;

// ---------------------------------------------------------------------------
// Shared gateway state
// ---------------------------------------------------------------------------

/// Shared runtime state for the gateway: the agent pool and per-request metrics.
pub struct GatewayState {
    pub pool: AgentPool,
    pub metrics: Metrics,
}

impl GatewayState {
    pub fn new(pool: AgentPool) -> Self {
        Self {
            pool,
            metrics: Metrics::default(),
        }
    }
}

/// Atomic counters for key gateway events, shared across request handlers.
#[derive(Default)]
pub struct Metrics {
    pub requests: AtomicU64,
    pub tool_calls: AtomicU64,
    pub denials: AtomicU64,
    pub errors: AtomicU64,
}

impl Metrics {
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            requests: self.requests.load(Ordering::Relaxed),
            tool_calls: self.tool_calls.load(Ordering::Relaxed),
            denials: self.denials.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub requests: u64,
    pub tool_calls: u64,
    pub denials: u64,
    pub errors: u64,
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

/// OpenAI-compatible chat completions request body.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionsRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: bool,
    /// Override the agent's instruction (a system message in the usual spot).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
}

/// A single message in the OpenAI-compatible format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// Pan-native goal request.
#[derive(Debug, Deserialize)]
pub struct GoalRequest {
    pub objective: String,
    #[serde(default)]
    pub trigger: TriggerInfo,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Deserialize)]
pub struct TriggerInfo {
    #[serde(default = "default_trigger_kind")]
    pub kind: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub content: String,
}

impl Default for TriggerInfo {
    fn default() -> Self {
        Self {
            kind: "utterance".into(),
            from: "user".into(),
            content: String::new(),
        }
    }
}

fn default_trigger_kind() -> String {
    "utterance".into()
}

/// Delegate a goal to a child agent. Used by `/v1/agents/{parent}/delegate`.
#[derive(Debug, Deserialize)]
pub struct DelegateRequest {
    /// The agent name (child) to delegate to.
    pub agent: String,
    /// The objective for the child.
    pub objective: String,
    /// Optional trigger for the child (defaults to utterance).
    #[serde(default)]
    pub trigger: Option<TriggerInfo>,
    /// Override the child's instruction.
    #[serde(default)]
    pub instruction: Option<String>,
    /// Whether to stream the child's response.
    #[serde(default)]
    pub stream: bool,
}

/// The response returned (or streamed) by the gateway.
#[derive(Debug, Serialize)]
pub struct GatewayResponse {
    pub expressed: Vec<String>,
    pub results: Vec<Value>,
    pub end: String,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// Build the gateway router.
pub fn router(state: Arc<GatewayState>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/agents/{name}/goals", post(agent_goals))
        .route("/v1/agents/{name}/delegate", post(agent_delegate))
        .route("/v1/agents", get(list_agents))
        .route("/v1/metrics", get(metrics_handler))
        .route("/health", get(health))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn metrics_handler(State(state): State<Arc<GatewayState>>) -> Json<MetricsSnapshot> {
    Json(state.metrics.snapshot())
}

async fn list_agents(State(state): State<Arc<GatewayState>>) -> Json<Vec<String>> {
    Json(state.pool.names().map(|n| n.to_string()).collect())
}

async fn chat_completions(
    State(state): State<Arc<GatewayState>>,
    Json(req): Json<ChatCompletionsRequest>,
) -> Result<Response, StatusCode> {
    let agent = state.pool.get(&req.model).ok_or_else(|| {
        tracing::warn!(model = %req.model, "agent not found");
        StatusCode::NOT_FOUND
    })?;
    state.metrics.requests.fetch_add(1, Ordering::Relaxed);

    let (goal, ctx) = messages_to_goal_and_context(&req.messages, &req);
    let report = run_agent(agent, goal, ctx, &state.metrics).await;

    if req.stream {
        Ok(Sse::new(report_to_sse_stream(report))
            .keep_alive(KeepAlive::default())
            .into_response())
    } else {
        Ok(Json(to_gateway_response(report)).into_response())
    }
}

async fn agent_goals(
    State(state): State<Arc<GatewayState>>,
    Path(name): Path<String>,
    Json(req): Json<GoalRequest>,
) -> Result<Response, StatusCode> {
    let agent = state.pool.get(&name).ok_or_else(|| {
        tracing::warn!(agent = %name, "agent not found");
        StatusCode::NOT_FOUND
    })?;
    state.metrics.requests.fetch_add(1, Ordering::Relaxed);

    let objective = req.objective;
    let goal = Goal {
        id: format!(
            "goal-{}",
            std::time::UNIX_EPOCH
                .elapsed()
                .unwrap_or_default()
                .as_nanos()
        ),
        revision: 0,
        objective: objective.clone(),
        trigger: Trigger::Utterance {
            from: if req.trigger.from.is_empty() {
                "user".into()
            } else {
                req.trigger.from
            },
            content: if req.trigger.content.is_empty() {
                objective
            } else {
                req.trigger.content
            },
        },
    };

    let report = run_agent(agent, goal, Context::default(), &state.metrics).await;

    if req.stream {
        Ok(Sse::new(report_to_sse_stream(report))
            .keep_alive(KeepAlive::default())
            .into_response())
    } else {
        Ok(Json(to_gateway_response(report)).into_response())
    }
}

async fn agent_delegate(
    State(state): State<Arc<GatewayState>>,
    Path(parent_name): Path<String>,
    Json(req): Json<DelegateRequest>,
) -> Result<Response, StatusCode> {
    let parent = state.pool.get(&parent_name).ok_or_else(|| {
        tracing::warn!(agent = %parent_name, "parent agent not found");
        StatusCode::NOT_FOUND
    })?;
    // Re-assemble the child with a delegation-narrowed scope.
    // The child's Agent.toml is loaded fresh so its provider/toolbox are correct;
    // only the scope is overridden to reflect the delegation chain.
    let agent_dir = state
        .pool
        .agent_dir()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let child_toml = agent_dir.join(format!("{}.toml", req.agent));
    let manifest = pan_agent::manifest::AgentManifest::load(&child_toml).map_err(|_| {
        tracing::warn!(agent = %req.agent, "child agent manifest not found");
        StatusCode::NOT_FOUND
    })?;
    let registry = pan_agent::builtin::builtin_registry();
    let mut child = pan_agent::assemble(&manifest, &registry).map_err(|e| {
        tracing::error!(error = %e, "failed to assemble child agent");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    // The child's governor was built from its own [caps.grant]. We narrow
    // its scope origin so the governor's grant table applies under a
    // sub-origin. The governor must have an entry for this origin or the
    // child is denied everything.
    child.scope = Scope::new(format!("{}.delegated.{}", parent.scope.origin, req.agent));
    state.metrics.requests.fetch_add(1, Ordering::Relaxed);

    let objective = req.objective;
    let goal = Goal {
        id: format!(
            "delegate-{}",
            std::time::UNIX_EPOCH
                .elapsed()
                .unwrap_or_default()
                .as_nanos()
        ),
        revision: 0,
        objective: objective.clone(),
        trigger: Trigger::Utterance {
            from: parent_name,
            content: objective,
        },
    };

    let report = run_agent(&child, goal, Context::default(), &state.metrics).await;

    if req.stream {
        Ok(Sse::new(report_to_sse_stream(report))
            .keep_alive(KeepAlive::default())
            .into_response())
    } else {
        Ok(Json(to_gateway_response(report)).into_response())
    }
}

// ---------------------------------------------------------------------------
// Core: run one agent span
// ---------------------------------------------------------------------------

async fn run_agent(
    agent: &AssembledAgent,
    goal: Goal,
    ctx: Context,
    _metrics: &Metrics,
) -> RunReport {
    let registry = agent.toolbox.registry();
    let mut stream = EventStream::spawn(TracingSink);
    let pipeline = Pipeline {
        registry: &registry,
        governor: &agent.governor,
        executor: &agent.toolbox,
        events: &stream,
    };
    let lp = Loop {
        provider: agent.provider.as_ref(),
        pipeline: &pipeline,
        events: &stream,
        scope: agent.scope.clone(),
    };
    let mut obs = Once(Some(goal));
    let report = lp.run_span(&mut obs, &ctx).await;
    stream.shutdown();
    report
}

/// Convert a completed [`RunReport`] into a stream of SSE events.
/// Each `Express` body becomes one `token` event; a final `done` event carries
/// the full report as JSON. True per-token streaming (emit as the loop runs)
/// requires a core-loop callback extension (ROADMAP Sprint 6B).
fn report_to_sse_stream(
    report: RunReport,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    let mut events = Vec::new();
    for body in &report.expressed {
        let payload = serde_json::json!({
            "type": "token",
            "content": body,
        });
        events.push(Ok(Event::default().data(payload.to_string())));
    }
    let done = serde_json::json!(to_gateway_response(report));
    events.push(Ok(Event::default().event("done").data(done.to_string())));
    futures::stream::iter(events)
}

// ---------------------------------------------------------------------------
// Request conversion helpers
// ---------------------------------------------------------------------------

/// Convert OpenAI-compatible messages into a Goal + Context.
/// The last user message becomes the current goal's trigger; all prior messages
/// (including system, user, and assistant turns) become history fragments.
fn messages_to_goal_and_context(
    messages: &[Message],
    req: &ChatCompletionsRequest,
) -> (Goal, Context) {
    // Use the agent's instruction, overridden by the request's instruction,
    // or by the last system message.
    let mut instruction = req.instruction.clone().unwrap_or_default();

    // Separate system messages from conversation history.
    let mut history: Vec<Value> = Vec::new();
    let mut last_user_content = String::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" => {
                instruction = msg.content.clone();
            }
            "user" => {
                last_user_content = msg.content.clone();
                history.push(serde_json::json!({"role": "user", "content": msg.content}));
            }
            "assistant" => {
                history.push(serde_json::json!({"role": "assistant", "content": msg.content}));
            }
            _ => {} // tool, function, etc. ignored
        }
    }

    let goal = Goal {
        id: format!(
            "chat-{}",
            std::time::UNIX_EPOCH
                .elapsed()
                .unwrap_or_default()
                .as_nanos()
        ),
        revision: 0,
        objective: last_user_content.clone(),
        trigger: Trigger::Utterance {
            from: "user".into(),
            content: last_user_content,
        },
    };

    let mut ctx = Context::default();
    // If there's a custom instruction, inject it as a persona fragment.
    if !instruction.trim().is_empty() {
        ctx = ctx.with("persona", instruction);
    }
    // Conversation history for the LLM provider's `history` channel.
    if !history.is_empty() {
        // Remove the last entry (it's the current user turn, not history).
        history.pop();
        if !history.is_empty() {
            ctx = ctx.with("history", serde_json::to_string(&history).unwrap());
        }
    }

    (goal, ctx)
}

fn to_gateway_response(report: RunReport) -> GatewayResponse {
    GatewayResponse {
        expressed: report.expressed,
        results: report.results.into_iter().map(|(_, r)| r).collect(),
        end: match report.end {
            Some(RunEnd::Concluded(Outcome::Achieved)) => "achieved".into(),
            Some(RunEnd::Concluded(Outcome::Abandoned | Outcome::Continue)) => "abandoned".into(),
            Some(RunEnd::StepLimit) => "step_limit".into(),
            Some(RunEnd::Abandoned) => "abandoned".into(),
            Some(RunEnd::StreamExhausted) => "exhausted".into(),
            None => "unknown".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pan_core::loop_engine::{RunEnd, RunReport};
    use pan_core::schema::{Outcome, Trigger};

    // -----------------------------------------------------------------------
    // messages_to_goal_and_context
    // -----------------------------------------------------------------------

    #[test]
    fn single_user_message_becomes_goal() {
        let messages = vec![Message {
            role: "user".into(),
            content: "Hello".into(),
        }];
        let req = ChatCompletionsRequest {
            model: "echo".into(),
            messages: vec![],
            stream: false,
            instruction: None,
        };
        let (goal, ctx) = messages_to_goal_and_context(&messages, &req);
        assert_eq!(goal.objective, "Hello");
        assert_eq!(
            goal.trigger,
            Trigger::Utterance {
                from: "user".into(),
                content: "Hello".into(),
            }
        );
        assert!(ctx.fragments.is_empty(), "no history or instruction");
    }

    #[test]
    fn system_message_becomes_persona_instruction() {
        let messages = vec![
            Message {
                role: "system".into(),
                content: "You are a helpful assistant.".into(),
            },
            Message {
                role: "user".into(),
                content: "Hi".into(),
            },
        ];
        let req = ChatCompletionsRequest {
            model: "echo".into(),
            messages: vec![],
            stream: false,
            instruction: None,
        };
        let (goal, ctx) = messages_to_goal_and_context(&messages, &req);
        assert_eq!(goal.objective, "Hi");
        assert_eq!(
            ctx.fragments[0].channel, "persona",
            "system message becomes persona fragment"
        );
        assert_eq!(ctx.fragments[0].body, "You are a helpful assistant.");
    }

    #[test]
    fn conversation_history_is_injected_as_context_fragment() {
        let messages = vec![
            Message {
                role: "system".into(),
                content: "You are helpful.".into(),
            },
            Message {
                role: "user".into(),
                content: "What's the weather?".into(),
            },
            Message {
                role: "assistant".into(),
                content: "It's sunny.".into(),
            },
            Message {
                role: "user".into(),
                content: "Great!".into(),
            },
        ];
        let req = ChatCompletionsRequest {
            model: "echo".into(),
            messages: vec![],
            stream: false,
            instruction: None,
        };
        let (_goal, ctx) = messages_to_goal_and_context(&messages, &req);
        let history_frag = ctx
            .fragments
            .iter()
            .find(|f| f.channel == "history")
            .expect("history fragment should exist");
        let history: Vec<Value> =
            serde_json::from_str(&history_frag.body).expect("history is valid JSON");
        assert_eq!(history.len(), 2, "prior user + assistant turns");
        assert_eq!(history[0]["role"], "user");
        assert_eq!(history[0]["content"], "What's the weather?");
        assert_eq!(history[1]["role"], "assistant");
        assert_eq!(history[1]["content"], "It's sunny.");
    }

    #[test]
    fn request_instruction_used_when_no_system_message() {
        let messages = vec![Message {
            role: "user".into(),
            content: "Hi".into(),
        }];
        let req = ChatCompletionsRequest {
            model: "echo".into(),
            messages: vec![],
            stream: false,
            instruction: Some("Use this instruction.".into()),
        };
        let (_goal, ctx) = messages_to_goal_and_context(&messages, &req);
        assert_eq!(
            ctx.fragments[0].body, "Use this instruction.",
            "request instruction is used when there is no system message"
        );
    }

    #[test]
    fn system_message_overrides_request_instruction_when_present() {
        let messages = vec![
            Message {
                role: "system".into(),
                content: "Be ignored.".into(),
            },
            Message {
                role: "user".into(),
                content: "Hi".into(),
            },
        ];
        let req = ChatCompletionsRequest {
            model: "echo".into(),
            messages: vec![],
            stream: false,
            instruction: Some("Request instruction.".into()),
        };
        let (_goal, ctx) = messages_to_goal_and_context(&messages, &req);
        assert_eq!(
            ctx.fragments[0].body, "Be ignored.",
            "system message in messages array overrides request instruction"
        );
    }

    #[test]
    fn tool_messages_are_ignored() {
        let messages = vec![
            Message {
                role: "user".into(),
                content: "Check weather".into(),
            },
            Message {
                role: "tool".into(),
                content: "{\"temp\": 72}".into(),
            },
            Message {
                role: "assistant".into(),
                content: "72 degrees.".into(),
            },
        ];
        let req = ChatCompletionsRequest {
            model: "echo".into(),
            messages: vec![],
            stream: false,
            instruction: None,
        };
        let (goal, ctx) = messages_to_goal_and_context(&messages, &req);
        assert_eq!(goal.objective, "Check weather");
        let history_frag = ctx.fragments.iter().find(|f| f.channel == "history");
        assert!(
            history_frag.is_some(),
            "user+assistant history should survive"
        );
        let history: Vec<Value> = serde_json::from_str(&history_frag.unwrap().body).unwrap();
        assert_eq!(
            history.len(),
            1,
            "the tool message was skipped, leaving one entry"
        );
        assert_eq!(history[0]["role"], "user");
        assert_eq!(history[0]["content"], "Check weather");
    }

    #[test]
    fn no_history_when_only_one_user_message() {
        let messages = vec![Message {
            role: "user".into(),
            content: "Only me.".into(),
        }];
        let req = ChatCompletionsRequest {
            model: "echo".into(),
            messages: vec![],
            stream: false,
            instruction: None,
        };
        let (_goal, ctx) = messages_to_goal_and_context(&messages, &req);
        assert!(
            ctx.fragments.iter().all(|f| f.channel != "history"),
            "no history fragment for single message"
        );
    }

    // -----------------------------------------------------------------------
    // to_gateway_response
    // -----------------------------------------------------------------------

    fn test_run_report(
        expressed: Vec<String>,
        results: Vec<(String, Value)>,
        end: Option<RunEnd>,
    ) -> RunReport {
        RunReport {
            effected: vec![],
            failed: vec![],
            expressed,
            results,
            end,
        }
    }

    #[test]
    fn achieved_outcome_maps_to_achieved() {
        let report = test_run_report(
            vec!["Hello!".into()],
            vec![],
            Some(RunEnd::Concluded(Outcome::Achieved)),
        );
        let resp = to_gateway_response(report);
        assert_eq!(resp.end, "achieved");
        assert_eq!(resp.expressed, vec!["Hello!"]);
    }

    #[test]
    fn abandoned_outcome_maps_to_abandoned() {
        for outcome in [Outcome::Abandoned, Outcome::Continue] {
            let report = test_run_report(vec![], vec![], Some(RunEnd::Concluded(outcome)));
            let resp = to_gateway_response(report);
            assert_eq!(resp.end, "abandoned");
        }
    }

    #[test]
    fn step_limit_maps_to_step_limit() {
        let report = test_run_report(vec![], vec![], Some(RunEnd::StepLimit));
        let resp = to_gateway_response(report);
        assert_eq!(resp.end, "step_limit");
    }

    #[test]
    fn no_end_maps_to_unknown() {
        let report = test_run_report(vec![], vec![], None);
        let resp = to_gateway_response(report);
        assert_eq!(resp.end, "unknown");
    }

    #[test]
    fn results_are_carried_through() {
        let report = test_run_report(
            vec![],
            vec![("cap.shell".into(), Value::String("output".into()))],
            Some(RunEnd::Concluded(Outcome::Achieved)),
        );
        let resp = to_gateway_response(report);
        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0], Value::String("output".into()));
    }

    // -----------------------------------------------------------------------
    // report_to_sse_stream
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn sse_emits_token_events_then_done() {
        use futures::StreamExt;

        let report = test_run_report(
            vec!["Hello.".into(), "How are you?".into()],
            vec![],
            Some(RunEnd::Concluded(Outcome::Achieved)),
        );
        let stream = report_to_sse_stream(report);
        let events: Vec<_> = stream.collect().await;

        // Two token events plus one done event.
        assert_eq!(events.len(), 3, "expected 3 SSE events");
        // All events are Ok (Infallible).
        for e in &events {
            assert!(e.is_ok(), "SSE event should be Ok");
        }
    }
}
