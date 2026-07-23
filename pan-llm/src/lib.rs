//! # pan-llm — tool-using LLM providers.
//!
//! A [`Provider`](pan_core::schema::Provider) whose brain is a real model, built
//! to *use* tools through pan-core's ReAct loop rather than merely name one. It
//! maps the agent's capabilities to the chat API's function schema, turns a model
//! `tool_calls` reply into governed `Invoke`s, and reads the executed results
//! back off the loop's `tool_result` channel — all while staying just another
//! `Provider` behind the same vocabulary (no chat-shaped types leak into the
//! core).
//!
//! It speaks the OpenAI-compatible `/chat/completions` dialect over either plain
//! HTTP (local servers: Ollama, llama.cpp, LM Studio, a gateway) or TLS (cloud
//! BYOK: OpenAI, OpenRouter, Groq, Together, an Anthropic-compatible endpoint) —
//! the transport follows the `base` scheme (see [`http`]). Register it onto a
//! [`ComponentRegistry`] so an `Agent.toml` can select `provider = "provider.llm"`:
//!
//! ```toml
//! [persona]
//! provider = "provider.llm"
//! instruction = "You are a helpful assistant."
//! model = "llama3.2"
//! base = "http://127.0.0.1:11434/v1"
//! ```

pub mod anthropic;
pub mod compactor;
pub mod evaluator;
pub mod http;
pub mod openai;

use pan_core::schema::{ActionIntent, Decision, Outcome, Trigger};

// ---------------------------------------------------------------------------
// Shared utilities for LLM providers.
// ---------------------------------------------------------------------------

/// Build a terminal abandoned decision, logging the reason.
pub(crate) fn abandoned(prefix: &str, reason: &str) -> Decision {
    eprintln!("{prefix}: decide failed: {reason}");
    Decision {
        intents: vec![ActionIntent::Conclude {
            outcome: Outcome::Abandoned,
        }],
    }
}

/// Format a trigger as user-turn text for the LLM prompt.
pub(crate) fn trigger_to_text(trigger: &Trigger) -> String {
    match trigger {
        Trigger::Utterance { from, content } => format!("{from}: {content}"),
        Trigger::Event { topic, payload } => format!("(event: {topic} {payload})"),
        Trigger::Tick { .. } => "(a quiet moment passes)".to_string(),
        Trigger::Signal { name, value } => format!("(signal: {name} = {value})"),
    }
}

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;

use pan_core::components::{ComponentConfig, ComponentError, ComponentRegistry};
use pan_core::schema::Provider;

const DEFAULT_MAX_TOKENS: u32 = 512;
const DEFAULT_TEMPERATURE: f64 = 0.7;

/// Register the LLM provider components this crate offers. A stock binary calls
/// this on top of its base registry so `provider.llm` and `provider.anthropic`
/// are selectable from config.
pub fn register_llm_providers(registry: &mut ComponentRegistry) -> Result<(), ComponentError> {
    registry.register_provider("provider.llm", build_openai)?;
    registry.register_provider("provider.anthropic", build_anthropic)
}

/// Build a [`OpenAiProvider`] from `[persona]` settings, falling back to the
/// `PAN_LLM_*` environment for the endpoint bits so a key/URL need not be written
/// into the manifest. A missing `base` or `model` is a load-time error — an LLM
/// persona with nowhere to reach is a misconfiguration, not a silent no-op.
fn build_openai(cfg: &ComponentConfig) -> Result<Box<dyn Provider>, ComponentError> {
    let setting_str = |key: &str| cfg.settings.get(key).and_then(|v| v.as_str());

    let base = setting_str("base")
        .map(str::to_string)
        .or_else(|| std::env::var("PAN_LLM_BASE").ok())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ComponentError::Construction {
            id: cfg.id.clone(),
            reason: "provider.llm requires a `base` URL (or PAN_LLM_BASE)".into(),
        })?;

    let model = setting_str("model")
        .map(str::to_string)
        .or_else(|| std::env::var("PAN_LLM_MODEL").ok())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ComponentError::Construction {
            id: cfg.id.clone(),
            reason: "provider.llm requires a `model` (or PAN_LLM_MODEL)".into(),
        })?;

    let api_key = setting_str("api_key")
        .map(str::to_string)
        .or_else(|| std::env::var("PAN_LLM_API_KEY").ok())
        .filter(|s| !s.is_empty());

    let instruction = setting_str("instruction").unwrap_or("").to_string();
    let max_tokens = cfg
        .settings
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    let temperature = cfg
        .settings
        .get("temperature")
        .and_then(|v| v.as_f64())
        .unwrap_or(DEFAULT_TEMPERATURE);
    let token_budget = cfg.settings.get("token_budget").and_then(|v| v.as_u64());

    Ok(Box::new(OpenAiProvider {
        base,
        model,
        api_key,
        instruction,
        max_tokens,
        temperature,
        token_budget,
        tokens_used: std::sync::atomic::AtomicU64::new(0),
    }))
}

/// Build an [`AnthropicProvider`] from `[persona]` settings, with env fallback
/// (`PAN_ANTHROPIC_API_KEY`, `PAN_LLM_BASE`, `PAN_LLM_MODEL`).
fn build_anthropic(cfg: &ComponentConfig) -> Result<Box<dyn Provider>, ComponentError> {
    let setting_str = |key: &str| cfg.settings.get(key).and_then(|v| v.as_str());

    let base = setting_str("base")
        .map(str::to_string)
        .or_else(|| std::env::var("PAN_LLM_BASE").ok())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ComponentError::Construction {
            id: cfg.id.clone(),
            reason: "provider.anthropic requires a `base` URL (or PAN_LLM_BASE)".into(),
        })?;

    let model = setting_str("model")
        .map(str::to_string)
        .or_else(|| std::env::var("PAN_LLM_MODEL").ok())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ComponentError::Construction {
            id: cfg.id.clone(),
            reason: "provider.anthropic requires a `model` (or PAN_LLM_MODEL)".into(),
        })?;

    let api_key = setting_str("api_key")
        .map(str::to_string)
        .or_else(|| std::env::var("PAN_ANTHROPIC_API_KEY").ok())
        .or_else(|| std::env::var("PAN_LLM_API_KEY").ok())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ComponentError::Construction {
            id: cfg.id.clone(),
            reason: "provider.anthropic requires an `api_key` (or PAN_ANTHROPIC_API_KEY / PAN_LLM_API_KEY)".into(),
        })?;

    let instruction = setting_str("instruction").unwrap_or("").to_string();
    let max_tokens = cfg
        .settings
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(1024);
    let token_budget = cfg.settings.get("token_budget").and_then(|v| v.as_u64());

    Ok(Box::new(AnthropicProvider {
        base,
        model,
        api_key,
        instruction,
        max_tokens,
        token_budget,
        tokens_used: std::sync::atomic::AtomicU64::new(0),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn missing_base_is_a_load_error() {
        // Clear any ambient env so the manifest-only path is what's under test.
        std::env::remove_var("PAN_LLM_BASE");
        let mut reg = ComponentRegistry::new();
        register_llm_providers(&mut reg).unwrap();
        let built = reg.build_provider(&ComponentConfig::new(
            "provider.llm",
            serde_json::json!({ "model": "m" }),
        ));
        assert!(matches!(built, Err(ComponentError::Construction { .. })));
    }

    #[test]
    fn builds_from_full_settings() {
        let mut reg = ComponentRegistry::new();
        register_llm_providers(&mut reg).unwrap();
        let built = reg.build_provider(&ComponentConfig::new(
            "provider.llm",
            serde_json::json!({
                "base": "http://127.0.0.1:11434/v1",
                "model": "llama3.2",
                "instruction": "Be helpful.",
                "max_tokens": 128,
                "temperature": 0.5
            }),
        ));
        let provider = built.expect("should build from complete settings");
        assert_eq!(provider.id(), "provider.llm");
    }
}
