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

/// Build the registry of components this binary knows how to construct.
pub fn builtin_registry() -> ComponentRegistry {
    let mut reg = ComponentRegistry::new();
    // These registrations are internal and fixed, so the conflict errors cannot
    // fire; unwrap documents that.
    reg.register_provider("provider.rules", build_rules)
        .expect("unique builtin id");
    reg.register_provider("provider.behaviortree", build_behaviortree)
        .expect("unique builtin id");
    reg
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
    }

    #[test]
    fn rules_provider_builds_with_no_settings() {
        let reg = builtin_registry();
        let built = reg.build_provider(&ComponentConfig::bare("provider.rules"));
        assert!(built.is_ok(), "an empty rules provider is valid");
    }
}
