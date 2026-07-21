//! # `provider.anthropic` — a tool-using Anthropic Messages API provider.
//!
//! Same [`Provider`] contract as `openai.rs`: maps capabilities to `tools`,
//! turns a `tool_use` content block into `Invoke`s, and rides the ReAct loop.
//! Uses `x-api-key` + `anthropic-version` headers instead of Bearer auth.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pan_core::loop_engine::TOOL_RESULT_CHANNEL;
use pan_core::schema::{
    ActionIntent, Capability, Context, Decision, Goal, Outcome, Provider, Trigger, Value,
};

use crate::http;

const HTTP_TIMEOUT: Duration = Duration::from_secs(90);
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    pub base: String,
    pub model: String,
    pub api_key: String,
    pub instruction: String,
    pub max_tokens: u32,
    pub token_budget: Option<u64>,
    /// Cumulative tokens used across all decides.
    pub tokens_used: AtomicU64,
}

#[async_trait::async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        "provider.anthropic"
    }

    async fn decide(&self, goal: &Goal, ctx: &Context, caps: &[Capability]) -> Decision {
        if let Some(budget) = self.token_budget {
            if self.tokens_used.load(Ordering::Relaxed) >= budget {
                return abandoned("token budget exhausted");
            }
        }
        let (tools, name_to_id) = tool_schema(caps);
        let (system, messages) = self.build_messages(goal, ctx);
        let mut request = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": messages,
        });
        if !system.is_empty() {
            request["system"] = Value::String(system);
        }
        if !tools.is_empty() {
            request["tools"] = Value::Array(tools);
        }

        let headers: &[(&str, String)] = &[
            ("x-api-key", self.api_key.clone()),
            ("anthropic-version", ANTHROPIC_VERSION.to_string()),
        ];
        match http::post_json_ex(&self.base, "/v1/messages", &request, headers, HTTP_TIMEOUT) {
            Ok(response) => {
                if let Some(usage) = response.get("usage") {
                    let input = usage
                        .get("input_tokens")
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0);
                    let output = usage
                        .get("output_tokens")
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0);
                    self.tokens_used
                        .fetch_add(input + output, Ordering::Relaxed);
                }
                interpret(&response, &name_to_id)
            }
            Err(e) => abandoned(&e),
        }
    }
}

impl AnthropicProvider {
    fn build_messages(&self, goal: &Goal, ctx: &Context) -> (String, Vec<Value>) {
        let mut system = self.instruction.trim().to_string();
        let mut history_msgs: Vec<Value> = Vec::new();
        // Separate channels: "history" → prior turns, "persona" + others → system
        for fragment in &ctx.fragments {
            if fragment.channel == TOOL_RESULT_CHANNEL {
                continue;
            }
            if fragment.channel == "history" {
                if let Ok(turns) = serde_json::from_str::<Vec<Value>>(&fragment.body) {
                    for turn in turns {
                        if let Some(role) = turn.get("role").and_then(|r| r.as_str()) {
                            if role == "user" || role == "assistant" {
                                history_msgs.push(turn);
                            }
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

        let user_content = user_turn(&goal.trigger);

        // Build the messages array
        let mut messages: Vec<Value> = Vec::new();

        // Prior history as user/assistant pairs
        for msg in &history_msgs {
            if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": msg.get("content").and_then(|c| c.as_str()).unwrap_or("")
                }));
            } else if msg.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": msg.get("content").and_then(|c| c.as_str()).unwrap_or("")
                }));
            }
        }

        // Current user turn
        messages.push(serde_json::json!({
            "role": "user",
            "content": user_content,
        }));

        // Tool result fragments — Anthropic puts these as `user` messages with
        // `tool_result` content blocks, interleaved with assistant `tool_use` blocks
        for fragment in &ctx.fragments {
            if fragment.channel != TOOL_RESULT_CHANNEL {
                continue;
            }
            if let Some((assistant_msg, tool_result_msg)) = replay_tool_exchange(&fragment.body) {
                messages.push(assistant_msg);
                messages.push(tool_result_msg);
            }
        }

        (system, messages)
    }
}

/// Turn a tool exchange fragment into the `(assistant tool_use, user tool_result)`
/// message pair that Anthropic expects.
fn replay_tool_exchange(body: &str) -> Option<(Value, Value)> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    let capability = parsed.get("capability")?.as_str()?;
    let id = parsed
        .get("correlation")
        .and_then(|c| c.as_str())
        .unwrap_or("toolu_0")
        .to_string();
    let arguments = parsed
        .get("args")
        .map(|a| a.to_string())
        .unwrap_or_else(|| "{}".to_string());
    let result_content = match (parsed.get("result"), parsed.get("error")) {
        (Some(result), _) => {
            let s = result.to_string();
            if s.len() > 32768 {
                format!("{}...[truncated: {} total chars]", &s[..32768], s.len())
            } else {
                s
            }
        }
        (None, Some(error)) => format!("error: {error}"),
        (None, None) => "null".to_string(),
    };

    let assistant = serde_json::json!({
        "role": "assistant",
        "content": [{
            "type": "tool_use",
            "id": id,
            "name": sanitize(capability),
            "input": arguments,
        }],
    });
    let tool_result = serde_json::json!({
        "role": "user",
        "content": [{
            "type": "tool_result",
            "tool_use_id": id,
            "content": result_content,
        }],
    });
    Some((assistant, tool_result))
}

fn tool_schema(caps: &[Capability]) -> (Vec<Value>, HashMap<String, String>) {
    let mut tools = Vec::with_capacity(caps.len());
    let mut name_to_id = HashMap::with_capacity(caps.len());
    for cap in caps {
        let name = sanitize(&cap.id);
        name_to_id.insert(name.clone(), cap.id.clone());
        tools.push(serde_json::json!({
            "name": name,
            "description": cap.summary,
            "input_schema": cap.args_schema,
        }));
    }
    (tools, name_to_id)
}

fn interpret(response: &Value, name_to_id: &HashMap<String, String>) -> Decision {
    if let Some(err) = response.get("error") {
        return abandoned(&format!("server error: {err}"));
    }
    let Some(content) = response.get("content").and_then(|c| c.as_array()) else {
        return abandoned("response missing content array");
    };

    let mut express_text: Option<String> = None;
    let mut invokes: Vec<ActionIntent> = Vec::new();

    for block in content {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        let prev = express_text.take().unwrap_or_default();
                        express_text = if prev.is_empty() {
                            Some(trimmed.to_string())
                        } else {
                            Some(format!("{prev} {trimmed}"))
                        };
                    }
                }
            }
            Some("tool_use") => {
                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let capability = name_to_id
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| name.to_string());
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                let correlation = block.get("id").and_then(|i| i.as_str()).map(str::to_string);
                invokes.push(ActionIntent::Invoke {
                    capability,
                    args: input,
                    correlation,
                });
            }
            _ => {}
        }
    }

    // Return intents in the correct order: Express first (if present), then Invokes.
    let mut intents = Vec::new();
    if let Some(text) = express_text {
        intents.push(ActionIntent::Express { body: text });
    }
    intents.extend(invokes);

    // If there were tool_use blocks, don't conclude — the ReAct loop continues.
    let has_tool_use = content
        .iter()
        .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"));
    if !has_tool_use {
        intents.push(ActionIntent::Conclude {
            outcome: Outcome::Achieved,
        });
    }
    // When tool_use is present, intents are already in order (text before invoke)
    // and the loop will not conclude — it re-decides on the same goal.

    Decision { intents }
}

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
    eprintln!("provider.anthropic: decide failed: {reason}");
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

    #[allow(dead_code)]
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

    #[allow(dead_code)]
    fn provider() -> AnthropicProvider {
        AnthropicProvider {
            base: "http://127.0.0.1:1".into(),
            model: "claude-sonnet-4-20250514".into(),
            api_key: "sk-test".into(),
            instruction: "You are a calculator.".into(),
            max_tokens: 128,
            token_budget: None,
            tokens_used: AtomicU64::new(0),
        }
    }

    #[test]
    fn tool_schema_uses_anthropic_shape() {
        let (tools, map) = tool_schema(&[cap("cap.state.get"), cap("cap.shell.run")]);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "cap_state_get");
        assert_eq!(tools[1]["input_schema"]["type"], "object");
        assert_eq!(map.get("cap_state_get").unwrap(), "cap.state.get");
    }

    #[test]
    fn tool_use_becomes_invoke_without_conclude() {
        let (_, map) = tool_schema(&[cap("cap.compute")]);
        let response = serde_json::json!({
            "content": [{
                "type": "tool_use",
                "id": "toolu_abc123",
                "name": "cap_compute",
                "input": { "x": 6, "y": 7 }
            }],
            "stop_reason": "tool_use",
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
                assert_eq!(correlation.as_deref(), Some("toolu_abc123"));
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
        assert_eq!(decision.outcome(), None);
    }

    #[test]
    fn plain_text_becomes_express_then_conclude() {
        let response = serde_json::json!({
            "content": [{
                "type": "text",
                "text": "The answer is 42."
            }],
            "stop_reason": "end_turn",
        });
        let decision = interpret(&response, &HashMap::new());
        assert_eq!(
            decision.intents,
            vec![
                ActionIntent::Express {
                    body: "The answer is 42.".into()
                },
                ActionIntent::Conclude {
                    outcome: Outcome::Achieved
                },
            ]
        );
    }

    #[test]
    fn text_and_tool_use_emits_both() {
        let (_, map) = tool_schema(&[cap("cap.compute")]);
        let response = serde_json::json!({
            "content": [
                { "type": "text", "text": "Let me calculate." },
                { "type": "tool_use", "id": "tu_1", "name": "cap_compute", "input": { "x": 6, "y": 7 } }
            ],
            "stop_reason": "tool_use",
        });
        let decision = interpret(&response, &map);
        // Express first, then Invoke (no Conclude)
        assert_eq!(decision.intents.len(), 2);
        assert!(matches!(&decision.intents[0], ActionIntent::Express { .. }));
        assert!(matches!(&decision.intents[1], ActionIntent::Invoke { .. }));
        assert_eq!(decision.outcome(), None);
    }

    #[test]
    fn server_error_becomes_abandoned() {
        let response = serde_json::json!({ "error": { "message": "overloaded" } });
        let decision = interpret(&response, &HashMap::new());
        assert_eq!(decision.outcome(), Some(Outcome::Abandoned));
    }
}
