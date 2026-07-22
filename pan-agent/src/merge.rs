use pan_core::config::Config;
use pan_core::schema::Value;

use crate::manifest::AgentManifest;

fn merge_global(global: &Config, id: &str, agent_settings: &toml::Table) -> Value {
    let mut merged = serde_json::Map::new();

    if let Some(table) = global.plugin.get(id) {
        for (k, v) in table {
            merged.insert(k.clone(), toml_value_to_json(v));
        }
    }

    for (k, v) in agent_settings {
        merged.insert(k.clone(), toml_value_to_json(v));
    }

    Value::Object(merged)
}

pub fn merge_provider_settings(global: Option<&Config>, manifest: &AgentManifest) -> Value {
    let mut agent_table = toml::Table::new();
    agent_table.insert(
        "instruction".into(),
        toml::Value::String(manifest.persona.instruction.clone()),
    );
    if let Some(model) = &manifest.persona.model {
        agent_table.insert("model".into(), toml::Value::String(model.clone()));
    }
    for (k, v) in &manifest.persona.settings {
        agent_table.insert(k.clone(), v.clone());
    }

    match global {
        Some(cfg) => merge_global(cfg, &manifest.persona.provider, &agent_table),
        None => toml_table_to_json(&agent_table),
    }
}

pub fn merge_cap_settings(
    global: Option<&Config>,
    manifest: &AgentManifest,
    cap_id: &str,
) -> Value {
    let agent_table = manifest
        .caps
        .settings
        .get(cap_id)
        .and_then(|v| v.as_table())
        .cloned()
        .unwrap_or_default();

    match global {
        Some(cfg) => merge_global(cfg, cap_id, &agent_table),
        None => toml_table_to_json(&agent_table),
    }
}

fn toml_table_to_json(table: &toml::Table) -> Value {
    let mut out = serde_json::Map::new();
    for (k, v) in table {
        out.insert(k.clone(), toml_value_to_json(v));
    }
    Value::Object(out)
}

fn toml_value_to_json(v: &toml::Value) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pan_core::config::Config;

    #[test]
    fn global_provider_settings_are_base_layer() {
        let mut global = Config::default();
        let mut table = toml::Table::new();
        table.insert("api_key".into(), toml::Value::String("sk-global".into()));
        table.insert("base".into(), toml::Value::String("http://global".into()));
        global.plugin.insert("provider.llm".into(), table);

        let manifest = AgentManifest::from_toml(
            r#"
[meta]
name = "test"
[persona]
provider = "provider.llm"
base = "http://agent"
"#,
        )
        .unwrap();

        let merged = merge_provider_settings(Some(&global), &manifest);
        let obj = merged.as_object().unwrap();
        assert_eq!(obj["base"], "http://agent");
        assert_eq!(obj["api_key"], "sk-global");
        assert!(obj.contains_key("instruction"));
    }

    #[test]
    fn no_global_settings_uses_agent_only() {
        let global = Config::default();
        let manifest = AgentManifest::from_toml(
            r#"
[meta]
name = "test"
[persona]
provider = "provider.echo"
prefix = "> "
"#,
        )
        .unwrap();

        let merged = merge_provider_settings(Some(&global), &manifest);
        let obj = merged.as_object().unwrap();
        assert_eq!(obj["prefix"], "> ");
        assert!(obj.contains_key("instruction"));
    }

    #[test]
    fn none_global_is_equivalent_to_empty() {
        let manifest = AgentManifest::from_toml(
            r#"
[meta]
name = "test"
[persona]
provider = "provider.echo"
prefix = "> "
"#,
        )
        .unwrap();

        let merged = merge_provider_settings(None, &manifest);
        let obj = merged.as_object().unwrap();
        assert_eq!(obj["prefix"], "> ");
    }

    #[test]
    fn capability_settings_merge() {
        let mut global = Config::default();
        let mut table = toml::Table::new();
        table.insert("root".into(), toml::Value::String("/global/fs".into()));
        global.plugin.insert("cap.fs".into(), table);

        let manifest = AgentManifest::from_toml(
            r#"
[meta]
name = "test"
[persona]
provider = "provider.echo"
[caps.settings."cap.fs"]
root = "/agent/fs"
"#,
        )
        .unwrap();

        let merged = merge_cap_settings(Some(&global), &manifest, "cap.fs");
        let obj = merged.as_object().unwrap();
        assert_eq!(obj["root"], "/agent/fs");
    }
}
