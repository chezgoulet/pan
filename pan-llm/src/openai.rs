//! # `provider.llm` — a tool-using, OpenAI-compatible chat provider.
//!
//! The same [`Provider`] trait as every other brain: `Goal` + `Context` +
//! capabilities in, [`Decision`] out. What makes it *agentic* is how it rides
//! pan-core's ReAct loop:
//!
//! - The agent's [`Capability`]s become the request's `tools` (function schema).
//! - A model `tool_calls` reply becomes `Invoke` intents (one per call), with the
//!   tool_call id carried as the intent's `correlation` and **no `Conclude`** — so
//!   the loop executes them, folds the results back onto
//!   [`TOOL_RESULT_CHANNEL`](pan_core::loop_engine::TOOL_RESULT_CHANNEL), and calls
//!   `decide` again.
//! - A plain text reply (no tool calls) becomes `Express` + `Conclude(Achieved)`.
//!
//! The provider is **stateless** across those `decide` calls: it reconstructs the
//! whole function-calling transcript (system, user, then each
//! `assistant(tool_call)` → `tool(result)` pair) from the goal and the
//! `tool_result` fragments the loop accumulated. That is exactly why the core
//! records the originating `args` in each fragment — without them the assistant
//! turn could not be rebuilt. No conversation state lives in the provider, so a
//! cancelled (superseded) `decide` leaves nothing behind.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pan_core::loop_engine::TOOL_RESULT_CHANNEL;
use pan_core::schema::{
    ActionIntent, Capability, Context, Decision, Goal, Outcome, Provider, Trigger, Value,
};

use crate::http;

const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

/// A chat provider speaking the OpenAI `/chat/completions` dialect with function
/// calling. Works against any compatible server (Ollama, llama.cpp, LM Studio, an
/// OpenAI-compatible gateway) reachable over plain HTTP.
pub struct OpenAiProvider {
    /// Base URL, e.g. `http://127.0.0.1:11434/v1` (local) or
    /// `https://api.openai.com/v1` (cloud); the `/chat/completions` path is
    /// appended. Both http and https (TLS) are supported — see [`crate::http`].
    pub base: String,
    /// Model id sent with every request.
    pub model: String,
    /// Optional bearer token (`Authorization: Bearer …`).
    pub api_key: Option<String>,
    /// The persona's system prompt (from `[persona] instruction`).
    pub instruction: String,
    pub max_tokens: u32,
    pub temperature: f64,
    /// Optional token budget. When set, the provider tracks usage via
    /// `usage.total_tokens` from the API and refuses new decisions once
    /// cumulative tokens exceed the budget.
    pub token_budget: Option<u64>,
    /// Cumulative tokens used across all decides, updated from the API
    /// response's `usage.total_tokens`. Read by the gateway's metrics.
    pub tokens_used: AtomicU64,
}

#[async_trait::async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> &str {
        "provider.llm"
    }

    async fn decide(&self, goal: &Goal, ctx: &Context, caps: &[Capability]) -> Decision {
        // Check token budget before making an API call.
        if let Some(budget) = self.token_budget {
            if self.tokens_used.load(Ordering::Relaxed) >= budget {
                return abandoned("token budget exhausted");
            }
        }

        let (tools, name_to_id) = tool_schema(caps);
        let messages = self.build_messages(goal, ctx);

        let mut request = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
        });
        if !tools.is_empty() {
            request["tools"] = Value::Array(tools);
            request["tool_choice"] = Value::String("auto".into());
        }

        // Blocking HTTP inside an async fn, on purpose: the loop's abandon-path
        // gives cancellation at the *future* level (a superseded goal drops this
        // whole future), matching `pan-daemon`'s llm client. A non-blocking client
        // is a later refinement.
        match http::post_json(
            &self.base,
            "/chat/completions",
            self.api_key.as_deref(),
            &request,
            HTTP_TIMEOUT,
        ) {
            Ok(response) => {
                // Track token usage from the API response.
                if let Some(usage) = response.get("usage") {
                    if let Some(tokens) = usage.get("total_tokens").and_then(|t| t.as_u64()) {
                        self.tokens_used.fetch_add(tokens, Ordering::Relaxed);
                    }
                }
                interpret(&response, &name_to_id)
            }
            Err(e) => abandoned(&e),
        }
    }
}

impl OpenAiProvider {
    /// Rebuild the full chat transcript for this step from the goal and context.
    /// Non-tool, non-history fragments (persona, memory, …) shape the system
    /// prompt; a `history` channel fragment (a JSON array of `{role, content}`
    /// turns) is replayed as prior user/assistant messages between the system
    /// prompt and the current user turn; each `tool_result` fragment replays as
    /// an `assistant(tool_call)` → `tool(result)` pair.
    fn build_messages(&self, goal: &Goal, ctx: &Context) -> Vec<Value> {
        let mut system = self.instruction.trim().to_string();
        let mut history_messages: Vec<Value> = Vec::new();
        for fragment in &ctx.fragments {
            if fragment.channel == TOOL_RESULT_CHANNEL {
                continue;
            }
            if fragment.channel == "history" {
                if let Ok(turns) = serde_json::from_str::<Vec<Value>>(&fragment.body) {
                    for turn in turns {
                        if matches!(
                            turn.get("role").and_then(|r| r.as_str()),
                            Some("user" | "assistant")
                        ) {
                            history_messages.push(turn);
                        }
                    }
                }
                continue;
            }
            if !system.is_empty() {
                system.push_str("\n\n");
            }
            system.push_str(&format!("[{}]\n{}", fragment.channel, fragment.body));
        }
        if !goal.objective.trim().is_empty() {
            if !system.is_empty() {
                system.push_str("\n\n");
            }
            system.push_str(&format!("[objective]\n{}", goal.objective.trim()));
        }

        let mut messages = vec![serde_json::json!({ "role": "system", "content": system })];
        messages.extend(history_messages);
        messages.push(serde_json::json!({ "role": "user", "content": user_turn(&goal.trigger) }));

        for fragment in &ctx.fragments {
            if fragment.channel != TOOL_RESULT_CHANNEL {
                continue;
            }
            if let Some((assistant, tool)) = replay_exchange(&fragment.body) {
                messages.push(assistant);
                messages.push(tool);
            }
        }
        messages
    }
}

/// Maximum characters for a single tool-output body replayed into the LLM
/// prompt. Outputs larger than this are truncated with a marker so the model
/// knows data was clipped rather than silently lost.
const MAX_TOOL_OUTPUT_CHARS: usize = 32_768;

/// Turn a `tool_result` fragment body into the `(assistant tool_call, tool
/// result)` message pair that must precede the model's next turn. Returns `None`
/// if the fragment is not the expected shape (then it is simply skipped).
fn replay_exchange(body: &str) -> Option<(Value, Value)> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    let capability = parsed.get("capability")?.as_str()?;
    // A synthetic id keeps the transcript valid even if a provider left
    // `correlation` unset (a non-LLM brain need not supply one).
    let id = parsed
        .get("correlation")
        .and_then(|c| c.as_str())
        .unwrap_or("call_0")
        .to_string();
    let arguments = parsed
        .get("args")
        .map(|a| a.to_string())
        .unwrap_or_else(|| "{}".to_string());
    let content = match (parsed.get("result"), parsed.get("error")) {
        (Some(result), _) => {
            let s = result.to_string();
            if s.len() > MAX_TOOL_OUTPUT_CHARS {
                format!(
                    "{}...[truncated: {} total chars]",
                    &s[..MAX_TOOL_OUTPUT_CHARS],
                    s.len()
                )
            } else {
                s
            }
        }
        (None, Some(error)) => format!("error: {error}"),
        (None, None) => "null".to_string(),
    };

    let assistant = serde_json::json!({
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": [{
            "id": id,
            "type": "function",
            "function": { "name": sanitize(capability), "arguments": arguments },
        }],
    });
    let tool = serde_json::json!({
        "role": "tool",
        "tool_call_id": id,
        "content": content,
    });
    Some((assistant, tool))
}

/// Build the `tools` array from the agent's capabilities, plus a map from each
/// sanitized function name back to its real capability id (OpenAI function names
/// forbid the `.` in capability ids, so `cap.state.get` is sent as
/// `cap_state_get` and mapped back on the way in).
fn tool_schema(caps: &[Capability]) -> (Vec<Value>, HashMap<String, String>) {
    let mut tools = Vec::with_capacity(caps.len());
    let mut name_to_id = HashMap::with_capacity(caps.len());
    for cap in caps {
        let name = sanitize(&cap.id);
        name_to_id.insert(name.clone(), cap.id.clone());
        tools.push(serde_json::json!({
            "type": "function",
            "function": {
                "name": name,
                "description": cap.summary,
                "parameters": cap.args_schema,
            },
        }));
    }
    (tools, name_to_id)
}

/// Interpret a chat-completion response into a [`Decision`]. Tool calls become
/// `Invoke`s (no conclude → the ReAct loop continues); a plain answer becomes
/// `Express` + `Conclude(Achieved)`.
fn interpret(response: &Value, name_to_id: &HashMap<String, String>) -> Decision {
    if let Some(err) = response.get("error") {
        return abandoned(&format!("server error: {err}"));
    }
    let message = &response["choices"][0]["message"];
    let content = message
        .get("content")
        .and_then(|c| c.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let tool_calls = message.get("tool_calls").and_then(|t| t.as_array());
    if let Some(calls) = tool_calls.filter(|c| !c.is_empty()) {
        let mut intents = Vec::new();
        // Some models narrate before calling; keep that as a spoken line.
        if let Some(text) = content {
            intents.push(ActionIntent::Express { body: text.into() });
        }
        for call in calls {
            let function = &call["function"];
            let sent_name = function.get("name").and_then(|n| n.as_str()).unwrap_or("");
            // Map the sanitized name back to the real capability id; if the model
            // named something we don't recognize, pass it through so the pipeline
            // reports "not registered" and the model can recover next step.
            let capability = name_to_id
                .get(sent_name)
                .cloned()
                .unwrap_or_else(|| sent_name.to_string());
            let args = function
                .get("arguments")
                .and_then(|a| a.as_str())
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            let correlation = call.get("id").and_then(|i| i.as_str()).map(str::to_string);
            intents.push(ActionIntent::Invoke {
                capability,
                args,
                correlation,
            });
        }
        // Deliberately NO Conclude: the loop executes the calls, folds the results
        // back, and asks us again.
        return Decision { intents };
    }

    // No tool calls → a final answer.
    let mut intents = Vec::new();
    if let Some(text) = content {
        intents.push(ActionIntent::Express { body: text.into() });
    }
    intents.push(ActionIntent::Conclude {
        outcome: Outcome::Achieved,
    });
    Decision { intents }
}

/// OpenAI function names must match `^[a-zA-Z0-9_-]+$`; capability ids use dots.
/// Map `.` → `_` (reversed via the per-call `name_to_id` table).
fn sanitize(capability_id: &str) -> String {
    capability_id.replace('.', "_")
}

fn user_turn(trigger: &Trigger) -> String {
    match trigger {
        Trigger::Utterance { from, content } => format!("{from}: {content}"),
        Trigger::Event { topic, payload } => format!("(event: {topic} {payload})"),
        Trigger::Tick { .. } => "(a quiet moment passes)".to_string(),
        Trigger::Signal { name, value } => format!("(signal: {name} = {value})"),
    }
}

fn abandoned(reason: &str) -> Decision {
    eprintln!("provider.llm: decide failed: {reason}");
    Decision {
        intents: vec![ActionIntent::Conclude {
            outcome: Outcome::Abandoned,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(id: &str) -> Capability {
        Capability {
            id: id.into(),
            summary: format!("does {id}"),
            args_schema: serde_json::json!({ "type": "object" }),
        }
    }

    fn goal() -> Goal {
        Goal {
            id: "g".into(),
            revision: 0,
            objective: "Answer the question.".into(),
            trigger: Trigger::Utterance {
                from: "user".into(),
                content: "what is 6 times 7?".into(),
            },
        }
    }

    fn provider() -> OpenAiProvider {
        OpenAiProvider {
            base: "http://127.0.0.1:1".into(),
            model: "test".into(),
            api_key: None,
            instruction: "You are a calculator.".into(),
            max_tokens: 64,
            temperature: 0.0,
            token_budget: None,
            tokens_used: AtomicU64::new(0),
        }
    }

    #[test]
    fn tool_schema_sanitizes_and_maps_back() {
        let (tools, map) = tool_schema(&[cap("cap.state.get"), cap("cap.shell.run")]);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["function"]["name"], "cap_state_get");
        assert_eq!(map.get("cap_state_get").unwrap(), "cap.state.get");
        assert_eq!(map.get("cap_shell_run").unwrap(), "cap.shell.run");
    }

    #[test]
    fn tool_call_becomes_invoke_without_conclude() {
        let (_, map) = tool_schema(&[cap("cap.compute")]);
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": { "name": "cap_compute", "arguments": "{\"x\":6,\"y\":7}" }
                    }]
                }
            }]
        });
        let decision = interpret(&response, &map);
        assert_eq!(decision.intents.len(), 1);
        match &decision.intents[0] {
            ActionIntent::Invoke {
                capability,
                args,
                correlation,
            } => {
                assert_eq!(capability, "cap.compute");
                assert_eq!(args["x"], 6);
                assert_eq!(args["y"], 7);
                assert_eq!(correlation.as_deref(), Some("call_abc"));
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
        // No Conclude → the ReAct loop keeps going.
        assert_eq!(decision.outcome(), None);
    }

    #[test]
    fn plain_answer_becomes_express_then_conclude() {
        let response = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": "It is 42." } }]
        });
        let decision = interpret(&response, &HashMap::new());
        assert_eq!(
            decision.intents,
            vec![
                ActionIntent::Express {
                    body: "It is 42.".into()
                },
                ActionIntent::Conclude {
                    outcome: Outcome::Achieved
                },
            ]
        );
    }

    #[test]
    fn server_error_becomes_abandoned() {
        let response = serde_json::json!({ "error": { "message": "boom" } });
        let decision = interpret(&response, &HashMap::new());
        assert_eq!(decision.outcome(), Some(Outcome::Abandoned));
    }

    #[test]
    fn transcript_reconstructs_the_tool_exchange() {
        // A context carrying one persona fragment and one completed tool exchange
        // (as the loop would have folded it in) must rebuild to:
        // system, user, assistant(tool_call), tool(result).
        let ctx = Context::default().with("persona", "Be terse.").with(
            TOOL_RESULT_CHANNEL,
            serde_json::json!({
                "capability": "cap.compute",
                "correlation": "call_1",
                "args": { "x": 6, "y": 7 },
                "result": { "value": 42 }
            })
            .to_string(),
        );
        let messages = provider().build_messages(&goal(), &ctx);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "system");
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("Be terse."));
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("[objective]"));
        assert_eq!(messages[1]["role"], "user");
        assert!(messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("6 times 7"));
        // The reconstructed assistant tool-call carries the sanitized name + args.
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(
            messages[2]["tool_calls"][0]["function"]["name"],
            "cap_compute"
        );
        assert_eq!(messages[2]["tool_calls"][0]["id"], "call_1");
        // The tool result references the same id.
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call_1");
        assert!(messages[3]["content"].as_str().unwrap().contains("42"));
    }

    #[test]
    fn a_tool_error_replays_as_a_tool_message() {
        let body = serde_json::json!({
            "capability": "cap.fs.write",
            "correlation": "call_9",
            "args": { "path": "/etc/passwd" },
            "error": "governance rejected: Deny"
        })
        .to_string();
        let (assistant, tool) = replay_exchange(&body).unwrap();
        assert_eq!(
            assistant["tool_calls"][0]["function"]["name"],
            "cap_fs_write"
        );
        assert_eq!(tool["tool_call_id"], "call_9");
        assert!(tool["content"].as_str().unwrap().contains("rejected"));
    }
}
