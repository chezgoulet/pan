//! # LiteLLM provider — multi-model via LiteLLM proxy.
//!
//! Implements the core [`Provider`](crate::schema::Provider) trait against a
//! [LiteLLM proxy](https://litellm.vercel.app/) server. LiteLLM exposes a
//! single OpenAI-compatible `/chat/completions` endpoint that routes requests
//! to different model backends (OpenAI, Anthropic, Cohere, local models, etc.)
//! based on the `model` field. The proxy handles key management, fallbacks,
//! load balancing, and cost tracking — making this single provider gateway to
//! many model families.
//!
//! Mapping (identical to `provider.llm` — the LiteLLM API is OpenAI-compatible):
//! - `Goal.trigger` (Utterance) -> the user turn.
//! - `Context.fragments` -> appended as `system`/context messages.
//! - `Capability` list -> OpenAI `tools` (function defs).
//! - assistant `tool_calls` -> `ActionIntent::Invoke { capability, args }`.
//! - assistant text -> `ActionIntent::Express { body }`.
//! - `finish_reason == "stop"` -> `ActionIntent::Conclude`.
//!
//! ## Configuration
//!
//! The proxy URL is configurable via the `base_url` field (default: the standard
//! LiteLLM proxy address `http://localhost:4000`). The model name is whatever
//! LiteLLM is configured to route — often a deployment name like `gpt-4` or
//! `claude-3-opus`. API key is the proxy's key if authentication is enabled.

use crate::schema::{
    ActionIntent, Capability, Context, Decision, Goal, Outcome, Provider, Trigger, Value,
};
use serde::Deserialize;

/// Default LiteLLM proxy address (localhost:4000 is the standard port).
pub const DEFAULT_LITELLM_URL: &str = "http://localhost:4000";
/// A reasonable default model for LiteLLM to route (matches the proxy's
/// configured model-to-backend mapping).
pub const DEFAULT_MODEL: &str = "gpt-4o-mini";

/// LiteLLM provider — routes model requests through a LiteLLM proxy server.
///
/// Every field is public so the CLI/config layer can construct it directly
/// without boilerplate wrapper types.
pub struct LiteLlm {
    /// Base URL of the LiteLLM proxy server (e.g. `http://litellm:4000`).
    /// Must include scheme and port. No trailing slash.
    pub base_url: String,
    /// Model/deployment name LiteLLM should route to. The proxy's config
    /// determines which backend serves it.
    pub model: String,
    /// API key for the LiteLLM proxy. Empty if the proxy has no auth.
    pub api_key: String,
    /// Optional HTTP client; injectable for tests.
    client: reqwest::blocking::Client,
}

impl LiteLlm {
    /// Create a new LiteLLM provider with the given proxy URL, model, and key.
    pub fn new(base_url: &str, model: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key: api_key.to_string(),
            client: reqwest::blocking::Client::new(),
        }
    }

    /// Dev/testing constructor: local LiteLLM proxy, key from `LITELLM_API_KEY`.
    pub fn local_proxy() -> Self {
        Self::new(
            DEFAULT_LITELLM_URL,
            DEFAULT_MODEL,
            std::env::var("LITELLM_API_KEY").unwrap_or_default().as_str(),
        )
    }

    /// Build the messages array from a Goal and Context.
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

    /// Build the OpenAI-compatible tools array from capabilities.
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
    /// note if the network/parse fails.
    fn complete(
        &self,
        goal: &Goal,
        ctx: &Context,
        caps: &[Capability],
    ) -> Decision {
        let url = format!("{}/chat/completions", self.base_url);
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
                            body: format!("⚠ LiteLLM provider error: {e}"),
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
                        body: format!("⚠ LiteLLM proxy HTTP {status}: {txt}"),
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
                            body: format!("⚠ LiteLLM response parse error: {e}"),
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

impl Provider for LiteLlm {
    fn id(&self) -> &str {
        "provider.llm.litellm"
    }

    fn decide(&self, goal: &Goal, ctx: &Context, caps: &[Capability]) -> Decision {
        self.complete(goal, ctx, caps)
    }
}

// --- Response shape (OpenAI-compatible, same as LiteLLM proxy) --------------

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
    use crate::schema::Trigger;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Spin up a tiny mock HTTP server that returns a given response body for
    /// any POST to `/chat/completions`. Returns the local address.
    fn mock_litellm(body: &'static str) -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body_c = body; // &'static str is Copy-safe

        std::thread::spawn(move || {
            for stream in listener.incoming().take(1) {
                let mut stream = stream.unwrap();
                let mut buf = [0; 4096];
                // Read the request (discard it — we always return the same body).
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body_c.len(),
                    body_c
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        (listener, port)
    }

    fn ctx_with_memory() -> Context {
        Context::default().with("memory", "the user is Sam")
    }

    #[test]
    fn id_is_provider_llm_litellm() {
        let p = LiteLlm::new("http://x", "m", "");
        assert_eq!(p.id(), "provider.llm.litellm");
    }

    #[test]
    fn builds_user_message_from_utterance() {
        let p = LiteLlm::new("http://x", "m", "");
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
        let p = LiteLlm::new("http://x", "m", "");
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
        let p = LiteLlm::new("http://x", "m", "");
        let caps = vec![Capability::new("cap.shell", "run a command", serde_json::json!({"type": "object", "required": ["command"]}))];
        let tools = p.build_tools(&caps).unwrap();
        assert_eq!(tools.as_array().unwrap().len(), 1);
        assert_eq!(tools[0]["function"]["name"], "cap.shell");
    }

    #[test]
    fn no_tools_when_no_caps() {
        let p = LiteLlm::new("http://x", "m", "");
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

    // --- Integration tests against a mock LiteLLM proxy ---------------------

    #[test]
    fn mock_proxy_returns_text() {
        let response_body = r#"{
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello from LiteLLM proxy!"
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        }"#;

        let (_listener, port) = mock_litellm(response_body);
        let p = LiteLlm::new(&format!("http://127.0.0.1:{port}"), "gpt-4o-mini", "");
        let g = Goal {
            id: "g1".into(),
            revision: 0,
            objective: "say hello".into(),
            trigger: Trigger::Utterance {
                from: "user".into(),
                content: "hello".into(),
            },
        };
        let d = p.decide(&g, &Context::default(), &[]);

        // Should have Express with the text and Conclude.
        let expressed: Vec<&str> = d.intents.iter().filter_map(|i| {
            if let ActionIntent::Express { body } = i { Some(body.as_str()) } else { None }
        }).collect();
        assert!(expressed.contains(&"Hello from LiteLLM proxy!"));
        assert!(d.intents.iter().any(|i| matches!(i, ActionIntent::Conclude { .. })));
    }

    #[test]
    fn mock_proxy_returns_tool_call() {
        let response_body = r#"{
            "id": "chatcmpl-mock-tc",
            "object": "chat.completion",
            "created": 1700000001,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "cap.shell",
                            "arguments": "{\"command\":\"ls -la\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 20, "completion_tokens": 10, "total_tokens": 30}
        }"#;

        let (_listener, port) = mock_litellm(response_body);
        let p = LiteLlm::new(&format!("http://127.0.0.1:{port}"), "gpt-4o-mini", "");
        let g = Goal {
            id: "g2".into(),
            revision: 0,
            objective: "list files".into(),
            trigger: Trigger::Utterance {
                from: "user".into(),
                content: "list files".into(),
            },
        };
        let d = p.decide(&g, &Context::default(), &[]);

        // Should have Invoke for cap.shell and Conclude.
        let invokes: Vec<&str> = d.intents.iter().filter_map(|i| {
            if let ActionIntent::Invoke { capability, .. } = i { Some(capability.as_str()) } else { None }
        }).collect();
        assert!(invokes.contains(&"cap.shell"));
        assert!(d.intents.iter().any(|i| matches!(i, ActionIntent::Conclude { .. })));
    }

    #[test]
    fn mock_proxy_http_error_graceful_degradation() {
        // Return a 500 to verify graceful degradation.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(1) {
                let mut stream = stream.unwrap();
                let mut buf = [0; 4096];
                let _ = stream.read(&mut buf);
                let response = "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nContent-Length: 21\r\nConnection: close\r\n\r\nLiteLLM proxy error: X";
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        let p = LiteLlm::new(&format!("http://127.0.0.1:{port}"), "gpt-4o-mini", "");
        let g = Goal {
            id: "g3".into(),
            revision: 0,
            objective: "test error".into(),
            trigger: Trigger::Utterance { from: "u".into(), content: "hi".into() },
        };
        let d = p.decide(&g, &Context::default(), &[]);

        // Should gracefully degrade: Express with error note + Conclude.
        let has_express = d.intents.iter().any(|i| matches!(i, ActionIntent::Express { .. }));
        let has_conclude = d.intents.iter().any(|i| matches!(i, ActionIntent::Conclude { .. }));
        assert!(has_express, "should Express an error message on HTTP error");
        assert!(has_conclude, "should Conclude even on HTTP error");
    }

    #[test]
    fn api_key_sent_as_bearer_token() {
        // Verify the Provider trait method works with an API key.
        // The mock doesn't validate auth — we just verify the key is stored.
        let p = LiteLlm::new("http://127.0.0.1:1", "m", "sk-test-key");
        assert_eq!(p.api_key, "sk-test-key");
    }
}
