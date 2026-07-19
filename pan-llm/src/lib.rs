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
//! Today it targets **plain-HTTP** OpenAI-compatible servers (Ollama, llama.cpp,
//! LM Studio, a local gateway); cloud BYOK over TLS is an additive transport (see
//! [`http`]). Register it onto a [`ComponentRegistry`] so an `Agent.toml` can
//! select `provider = "provider.llm"`:
//!
//! ```toml
//! [persona]
//! provider = "provider.llm"
//! instruction = "You are a helpful assistant."
//! model = "llama3.2"
//! base = "http://127.0.0.1:11434/v1"
//! ```

pub mod http;
pub mod openai;

pub use openai::OpenAiProvider;

use pan_core::components::{ComponentConfig, ComponentError, ComponentRegistry};
use pan_core::schema::Provider;

const DEFAULT_MAX_TOKENS: u32 = 512;
const DEFAULT_TEMPERATURE: f64 = 0.7;

/// Register the LLM provider components this crate offers. A stock binary calls
/// this on top of its base registry so `provider.llm` is selectable from config.
pub fn register_llm_providers(registry: &mut ComponentRegistry) -> Result<(), ComponentError> {
    registry.register_provider("provider.llm", build_openai)
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

    Ok(Box::new(OpenAiProvider {
        base,
        model,
        api_key,
        instruction,
        max_tokens,
        temperature,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
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
