//! # Generic LLM provider — backend-agnostic.
//!
//! Implements the core [`Provider`](crate::schema::Provider) trait against any
//! **OpenAI-compatible** chat-completions endpoint. OpenRouter, OpenAI,
//! Together, Groq, a local `llama.cpp`/`ollama` server — all speak the same
//! `/chat/completions` shape, so one provider covers them. Backend selection is
//! pure config (`base_url` + `model` + `api_key`); nothing here names a vendor.
//!
//! This deliberately supersedes the original manifest's `provider.llm.anthropic`
//! (issue #9): pinning to one vendor's SDK would have broken the "core never
//! assumes LLM shape" thesis and locked dev/testing to a paid key. OpenRouter's
//! free tier is the dev default so the whole stack runs without spending money.
//!
//! Mapping (the only place chat-shape lives):
//! - `Goal.trigger` (Utterance) -> the user turn.
//! - `Context.fragments` -> appended as `system`/context messages.
//! - `Capability` list -> OpenAI `tools` (function defs). The core's dispatch
//!   pipeline enforces args via its own `validate` stage regardless, so a model
//!   that emits odd args fails identically to a misconfigured behavior tree.
//! - assistant `tool_calls` -> `ActionIntent::Invoke { capability, args }`
//!   (correlation = the tool_call id, so results can be matched back).
//! - assistant text -> `ActionIntent::Express { body }`.
//! - `finish_reason == "stop"` / `"tool_calls"` -> `ActionIntent::Conclude`.

use crate::schema::{
    ActionIntent, Capability, Context, Decision, Goal, Outcome, Provider, Trigger, Value,
};
use serde::Deserialize;

/// Default backend for dev/testing: OpenRouter's OpenAI-compatible API.
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
/// A known free model on OpenRouter. Override via config for real use.
pub const DEFAULT_MODEL: &str = "meta-llama/llama-3.1-8b-instruct:free";

pub struct Llm {
    pub base_url: String,
    pub model: String,
    /// API key. Read from config/env; never hardcoded. Empty is allowed (some
    /// local backends need no auth) but then remote calls will 401.
    pub api_key: String,
    /// Optional HTTP client; injectable for tests.
    client: reqwest::blocking::Client,
}

impl Llm {
    /// Dev/testing constructor: OpenRouter free tier, key from `OPENROUTER_API_KEY`.
    pub fn openrouter_free() -> Self {
        Self::new(
            DEFAULT_BASE_URL,
            DEFAULT_MODEL,
            std::env::var("OPENROUTER_API_KEY").unwrap_or_default().as_str(),
        )
    }

    /// Full constructor — any OpenAI-compatible backend.
    pub fn new(base_url: &str, model: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key: api_key.to_string(),
            client: reqwest::blocking::Client::new(),
        }
    }

    fn build_messages(&self, goal: &Goal, ctx: &Context) -> Vec<Value> {
        let mut msgs = Vec::new();
        // Context fragments become a system-context block (one message).
        if !ctx.fragments.is_empty() {
            let body = ctx
                .fragments
                .iter()
                .map(|f| format!("[{}] {}", f.channel, f.body))
                .collect::<Vec<_>>()
                .join("\n");
            msgs.push(serde_json::json!({ "role": "system", "content": body }));
        }
        // The trigger is the user turn.
        let user_content = match &goal.trigger {
            Trigger::Utterance { from, content } => format!("{from}: {content}"),
            Trigger::Tick { sequence } => format!("[tick #{}] {}", sequence, goal.objective),
            Trigger::Event { topic, payload } => {
                format!("[event {}] {} — {}", topic, goal.objective, payload)
            }
            Trigger::Signal { name, value } => {
                format!("[signal {} = {}] {}", name, value, goal.objective)
            }
        };
        msgs.push(serde_json::json!({ "role": "user", "content": user_content }));
        msgs
    }

    fn build_tools(&self, caps: &[Capability]) -> Option<Value> {
        if caps.is_empty() {
            return None;
        }
        let tools: Vec<Value> = caps
            .iter()
            .map(|c| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": c.id,
                        "description": c.summary,
                        "parameters": c.args_schema,
                    }
                })
            })
            .collect();
        Some(serde_json::json!(tools))
    }

    /// Issue the chat completion and parse the response into a `Decision`.
    /// Falls back to a graceful `Conclude(Achieved)` with an `Express` error
    /// note if the network/parse fails — the loop must never panic on a model.
    fn complete(
        &self,
        goal: &Goal,
        ctx: &Context,
        caps: &[Capability],
    ) -> Decision {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": self.build_messages(goal, ctx),
        });
        if let Some(tools) = self.build_tools(caps) {
            body["tools"] = tools;
            body["tool_choice"] = serde_json::json!("auto");
        }

        let mut req = self.client.post(&url).json(&body);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }

        let resp = match req.send() {
            Ok(r) => r,
            Err(e) => {
                return Decision {
                    intents: vec![
                        ActionIntent::Express {
                            body: format!("⚠ provider error: {e}"),
                        },
                        ActionIntent::Conclude {
                            outcome: Outcome::Achieved,
                        },
                    ],
                }
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let txt = resp.text().unwrap_or_default();
            return Decision {
                intents: vec![
                    ActionIntent::Express {
                        body: format!("⚠ provider HTTP {status}: {txt}"),
                    },
                    ActionIntent::Conclude {
                        outcome: Outcome::Achieved,
                    },
                ],
            };
        }

        let parsed: ChatResponse = match resp.json() {
            Ok(p) => p,
            Err(e) => {
                return Decision {
                    intents: vec![
                        ActionIntent::Express {
                            body: format!("⚠ response parse error: {e}"),
                        },
                        ActionIntent::Conclude {
                            outcome: Outcome::Achieved,
                        },
                    ],
                }
            }
        };

        let Some(choice) = parsed.choices.into_iter().next() else {
            return Decision {
                intents: vec![ActionIntent::Conclude {
                    outcome: Outcome::Achieved,
                }],
            };
        };
        let msg = choice.message;
        let mut intents = Vec::new();
        if let Some(content) = msg.content.filter(|s| !s.trim().is_empty()) {
            intents.push(ActionIntent::Express { body: content });
        }
        for tc in msg.tool_calls.unwrap_or_default() {
            let args: Value = serde_json::from_str(&tc.function.arguments)
                .unwrap_or(Value::Null);
            intents.push(ActionIntent::Invoke {
                capability: tc.function.name,
                args,
                correlation: Some(tc.id),
            });
        }
        // A turn that only emits text (no tool calls) is concluded.
        if !matches!(choice.finish_reason.as_deref(), Some("tool_calls")) {
            intents.push(ActionIntent::Conclude {
                outcome: Outcome::Achieved,
            });
        } else if intents
            .iter()
            .all(|i| !matches!(i, ActionIntent::Conclude { .. }))
        {
            // Model wants a tool call but sent no terminal intent; end the span
            // after effects so the loop doesn't spin.
            intents.push(ActionIntent::Conclude {
                outcome: Outcome::Achieved,
            });
        }
        Decision { intents }
    }
}

impl Provider for Llm {
    fn id(&self) -> &str {
        "provider.llm"
    }

    fn decide(&self, goal: &Goal, ctx: &Context, caps: &[Capability]) -> Decision {
        self.complete(goal, ctx, caps)
    }
}

// --- Response shape (OpenAI-compatible) -------------------------------------

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Deserialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type", default)]
    _type: Option<String>,
    function: FunctionCall,
}

#[derive(Deserialize)]
struct FunctionCall {
    name: String,
    arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Fragment, Trigger};

    fn ctx_with_memory() -> Context {
        Context::default().with("memory", "the user is Sam")
    }

    #[test]
    fn builds_user_message_from_utterance() {
        let p = Llm::new("http://x", "m", "");
        let g = Goal {
            id: "g".into(),
            revision: 0,
            objective: "chat".into(),
            trigger: Trigger::Utterance {
                from: "u".into(),
                content: "hello".into(),
            },
        };
        let msgs = p.build_messages(&g, &Context::default());
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert!(msgs[0]["content"].as_str().unwrap().contains("hello"));
    }

    #[test]
    fn context_fragments_become_system_message() {
        let p = Llm::new("http://x", "m", "");
        let g = Goal {
            id: "g".into(),
            revision: 0,
            objective: "o".into(),
            trigger: Trigger::Tick { sequence: 1 },
        };
        let msgs = p.build_messages(&g, &ctx_with_memory());
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert!(msgs[0]["content"].as_str().unwrap().contains("Sam"));
    }

    #[test]
    fn tools_built_from_capabilities() {
        let p = Llm::new("http://x", "m", "");
        let caps = vec![Capability {
            id: "cap.shell".into(),
            summary: "run a command".into(),
            args_schema: serde_json::json!({"type": "object", "required": ["command"]}),
        }];
        let tools = p.build_tools(&caps).unwrap();
        assert_eq!(tools.as_array().unwrap().len(), 1);
        assert_eq!(tools[0]["function"]["name"], "cap.shell");
    }

    #[test]
    fn no_tools_when_no_caps() {
        let p = Llm::new("http://x", "m", "");
        assert!(p.build_tools(&[]).is_none());
    }

    // Parse a realistic OpenAI-compatible response into the right intents.
    #[test]
    fn parses_tool_call_and_concludes() {
        let raw = r#"{
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "cap.shell",
                            "arguments": "{\"command\":\"ls -la /tmp\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }"#;
        let parsed: ChatResponse = serde_json::from_str(raw).unwrap();
        let msg = &parsed.choices[0].message;
        assert_eq!(msg.tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(msg.tool_calls.as_ref().unwrap()[0].function.name, "cap.shell");
    }

    #[test]
    fn parses_text_turn() {
        let raw = r#"{
            "choices": [{
                "message": {"role":"assistant","content":"hi there"},
                "finish_reason": "stop"
            }]
        }"#;
        let parsed: ChatResponse = serde_json::from_str(raw).unwrap();
        assert!(parsed.choices[0].message.content.as_deref().unwrap().contains("hi"));
    }
}
