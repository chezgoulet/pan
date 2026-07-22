//! # The built-in component set.
//!
//! [`builtin_registry`] is the [`ComponentRegistry`] a stock `pan` binary ships
//! with: the provider components that live in pan-core today. A deployment that
//! adds its own components (cloud LLM providers, channels, …) registers them on
//! top of this. `Agent.toml`'s `persona.provider` must name one of the ids here
//! (or one the binary added), or assembly fails with a clear load-time error.

use pan_core::components::{ComponentConfig, ComponentError, ComponentRegistry};
use pan_core::providers::behaviortree::{BehaviorTreeProvider, Node};
use pan_core::providers::rules::{Rule, RulesProvider};
use pan_core::schema::{Provider, Value};

use crate::echo::EchoProvider;

/// Build the registry of components a stock `pan` binary knows how to construct:
/// the pan-core providers, plus the `pan-cap` capability components (`cap.state`,
/// `cap.fs`). A deployment registers its own components on top.
pub fn builtin_registry() -> ComponentRegistry {
    let mut reg = ComponentRegistry::new();
    // These registrations are internal and fixed, so the conflict errors cannot
    // fire; expect documents that.
    reg.register_provider("provider.rules", build_rules)
        .expect("unique builtin id");
    reg.register_provider("provider.behaviortree", build_behaviortree)
        .expect("unique builtin id");
    reg.register_provider("provider.echo", build_echo)
        .expect("unique builtin id");
    reg.register_provider("provider.command", |_cfg| {
        Ok(Box::new(crate::command::CommandProvider::new()))
    })
    .expect("unique builtin id");
    // `provider.llm` — the tool-using LLM brain. Selectable from any Agent.toml;
    // it only reaches out to a model when a persona actually names it and supplies
    // a `base`/`model`, so the stock set stays dependency-free at rest.
    pan_llm::register_llm_providers(&mut reg).expect("unique builtin provider id");
    pan_cap::register_builtin_caps(&mut reg).expect("unique builtin capability ids");
    crate::agent::register_agent_cap(&mut reg);
    reg
}

/// `provider.echo` — replies to the user's utterance; an optional `prefix` from
/// `[persona] prefix = "…"` shapes the reply.
fn build_echo(cfg: &ComponentConfig) -> Result<Box<dyn Provider>, ComponentError> {
    let mut echo = EchoProvider::default();
    if let Some(prefix) = cfg.settings.get("prefix").and_then(|p| p.as_str()) {
        echo.prefix = prefix.to_string();
    }
    Ok(Box::new(echo))
}

/// `provider.rules` — parses an optional `rules` array out of settings; with no
/// rules it is a valid, quiet provider (every goal falls through to Continue).
fn build_rules(cfg: &ComponentConfig) -> Result<Box<dyn Provider>, ComponentError> {
    let rules = cfg
        .settings
        .get("rules")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(parse_rule).collect())
        .unwrap_or_default();
    Ok(Box::new(RulesProvider { rules }))
}

fn parse_rule(entry: &Value) -> Option<Rule> {
    let then = entry.get("then_invoke")?;
    let capability = then.get("capability")?.as_str()?.to_string();
    let args = then.get("args").cloned().unwrap_or(Value::Null);
    Some(Rule {
        when_signal_over: entry.get("when_signal_over").and_then(|s| {
            let name = s.get("name")?.as_str()?.to_string();
            let threshold = s.get("threshold")?.as_f64()?;
            Some((name, threshold))
        }),
        when_event_topic: entry
            .get("when_event_topic")
            .and_then(|t| t.as_str())
            .map(str::to_string),
        then_invoke: (capability, args),
    })
}

/// `provider.behaviortree` — an empty tree by default (no settings schema yet);
/// a real deployment supplies nodes. Present so the leak-test peer provider is
/// selectable from config.
fn build_behaviortree(_cfg: &ComponentConfig) -> Result<Box<dyn Provider>, ComponentError> {
    Ok(Box::new(BehaviorTreeProvider {
        root: vec![Node::Succeed],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_has_the_core_providers() {
        let reg = builtin_registry();
        let ids: Vec<&str> = reg.provider_ids().collect();
        assert!(ids.contains(&"provider.rules"));
        assert!(ids.contains(&"provider.behaviortree"));
        assert!(ids.contains(&"provider.llm"), "the LLM brain is selectable");
    }

    #[test]
    fn rules_provider_builds_with_no_settings() {
        let reg = builtin_registry();
        let built = reg.build_provider(&ComponentConfig::bare("provider.rules"));
        assert!(built.is_ok(), "an empty rules provider is valid");
    }
}
