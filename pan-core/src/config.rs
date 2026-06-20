//! # Configuration system — TOML with imports, env var expansion, PAN_ overrides.
//!
//! ## File format
//!
//! ```toml
//! import = ["base.toml", "overrides.toml"]
//!
//! [provider]
//! plugin = "provider.llm.anthropic"
//!
//! [plugins.memory.vector]
//! wasm = "~/.pan/plugins/pan-memory-ragamuffin.wasm"
//! enabled = true
//!
//! [plugins.provider.llm]
//! wasm = "~/.pan/plugins/pan-provider-llm.wasm"
//! api_key = "${OPENAI_API_KEY}"
//! ```
//!
//! ## Env var expansion
//!
//! Any TOML string value may contain `${VAR}` or `$VAR` references. These are
//! replaced with the value of the corresponding environment variable at load
//! time. Missing variables produce a `ConfigError::MissingEnvVar`.
//!
//! ## PAN_ overrides
//!
//! For any TOML key that maps to an environment variable `PAN_KEY_NAME` (with
//! dots and hyphens replaced by underscores, lowercased), the env var value
//! overrides the TOML value. Arrays and tables can be specified as JSON.
//!
//! ## Import resolution
//!
//! Imports are resolved relative to the importing file's directory. Cycles
//! produce a `ConfigError::ImportCycle`.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::env;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Parsed, validated, and fully-resolved Pan configuration.
///
/// Holds the processed TOML value tree plus canonical views for known sections.
#[derive(Debug, Clone)]
pub struct Config {
    /// Resolved TOML value tree (imports merged, vars expanded, overrides applied).
    pub raw: toml::Value,
    /// Plugin declarations. Keyed by plugin id, value is the plugin's config block.
    pub plugin_sections: HashMap<String, toml::Value>,
    /// Provider configuration, if declared.
    pub provider: Option<toml::Value>,
    /// All source files that contributed to this config (for diagnostics).
    pub sources: Vec<PathBuf>,
}

/// Known top-level config sections. The rest are treated as plugin sections.
const KNOWN_SECTIONS: &[&str] = &["import", "provider"];

impl Config {
    /// Load and fully resolve configuration from a file path.
    ///
    /// Stages:
    /// 1. Read root file → TOML value tree
    /// 2. Resolve imports recursively (relative to each file's directory)
    /// 3. Merge imported tables (later imports override earlier)
    /// 4. Expand `${VAR}` / `$VAR` references in all string values
    /// 5. Apply `PAN_*` environment variable overrides
    /// 6. Validate known sections, separate plugin sections
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let canonical = path.canonicalize().map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;

        let mut sources = Vec::new();
        let mut table = resolve_imports(&canonical, &mut sources, &mut BTreeSet::new())?;

        // Expand env var references.
        expand_env_vars(&mut table)?;

        // Apply PAN_ env overrides.
        apply_pan_overrides(&mut table)?;

        // Validate.
        validate(&table)?;

        // Separate known sections from plugin sections.
        let plugin_sections = extract_plugin_sections(&table);
        let provider = table.get("provider").cloned();

        Ok(Config {
            raw: toml::Value::Table(table),
            plugin_sections,
            provider,
            sources,
        })
    }
}

/// Look up a plugin-specific config block by its id.
pub fn plugin_config<'a>(config: &'a Config, plugin_id: &str) -> Option<&'a toml::Value> {
    config.plugin_sections.get(plugin_id)
}

// ---------------------------------------------------------------------------
// Import resolution
// ---------------------------------------------------------------------------

/// Recursively resolve imports. `visited` guards against cycles.
fn resolve_imports(
    path: &Path,
    sources: &mut Vec<PathBuf>,
    visited: &mut BTreeSet<PathBuf>,
) -> Result<toml::value::Table, ConfigError> {
    if !visited.insert(path.to_path_buf()) {
        return Err(ConfigError::ImportCycle { path: path.to_path_buf() });
    }

    let content = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;

    let mut table: toml::value::Table = toml::from_str(&content).map_err(|e| ConfigError::Parse {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;

    sources.push(path.to_path_buf());

    // Process imports.
    let import_paths: Vec<String> = table
        .remove("import")
        .and_then(|v| v.try_into().ok())
        .unwrap_or_default();

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    for rel in &import_paths {
        let abs = if rel.starts_with('/') {
            PathBuf::from(rel)
        } else {
            parent.join(rel)
        };
        let child = resolve_imports(&abs, sources, visited)?;
        // Merge: child keys override parent keys (later imports win).
        for (k, v) in child {
            table.insert(k, v);
        }
    }

    Ok(table)
}

// ---------------------------------------------------------------------------
// Env var expansion
// ---------------------------------------------------------------------------

/// Walk the TOML value tree and expand `${VAR}` and `$VAR` references.
fn expand_env_vars(value: &mut toml::Value) -> Result<(), ConfigError> {
    match value {
        toml::Value::String(s) => {
            *s = expand_string(s)?;
            Ok(())
        }
        toml::Value::Array(arr) => {
            for v in arr.iter_mut() {
                expand_env_vars(v)?;
            }
            Ok(())
        }
        toml::Value::Table(tbl) => {
            for v in tbl.values_mut() {
                expand_env_vars(v)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn expand_string(s: &str) -> Result<String, ConfigError> {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'$' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                // ${VAR} style
                let end = s[i + 2..].find('}').map(|p| i + 2 + p);
                match end {
                    Some(end) => {
                        let var = &s[i + 2..end];
                        let val = env::var(var).map_err(|_| ConfigError::MissingEnvVar {
                            name: var.to_string(),
                        })?;
                        result.push_str(&val);
                        i = end + 1;
                    }
                    None => {
                        // No closing brace — treat as literal
                        result.push(b'$' as char);
                        i += 1;
                    }
                }
            } else {
                // $VAR style — read until non-alphanumeric/underscore
                let start = i + 1;
                let end = s[start..]
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .map(|p| start + p)
                    .unwrap_or(s.len());

                if end > start {
                    let var = &s[start..end];
                    let val = env::var(var).map_err(|_| ConfigError::MissingEnvVar {
                        name: var.to_string(),
                    })?;
                    result.push_str(&val);
                    i = end;
                } else {
                    result.push(b'$' as char);
                    i += 1;
                }
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// PAN_ env var overrides
// ---------------------------------------------------------------------------

/// Apply `PAN_*` environment variable overrides to the TOML table.
///
/// Convention: `PAN_PROVIDER_MODEL` maps to `provider.model` in the config.
/// Arrays and tables are parsed from JSON; scalars use their raw string value.
fn apply_pan_overrides(table: &mut toml::value::Table) -> Result<(), ConfigError> {
    let prefix = "PAN_";
    let prefix_len = prefix.len();

    let mut vars: Vec<(String, String)> = Vec::new();
    for (key, val) in env::vars() {
        if key.starts_with(prefix) {
            vars.push((key[prefix_len..].to_string(), val));
        }
    }

    for (key, val) in &vars {
        // Convert PAN_KEY_NAME → toml key path
        let key_path: Vec<String> = key
            .split('_')
            .map(|s| s.to_lowercase())
            .collect();

        // Try JSON first (for arrays/tables), then use as raw string.
        let toml_val: toml::Value = serde_json::from_str(val)
            .map(|jv: serde_json::Value| toml_value_from_json(jv))
            .unwrap_or_else(|_| toml::Value::String(val.clone()));

        set_at_path(table, &key_path, toml_val);
    }

    Ok(())
}

fn toml_value_from_json(jv: serde_json::Value) -> toml::Value {
    match jv {
        serde_json::Value::Null => toml::Value::String("null".into()),
        serde_json::Value::Bool(b) => toml::Value::Boolean(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                toml::Value::Float(f)
            } else {
                toml::Value::String(n.to_string())
            }
        }
        serde_json::Value::String(s) => toml::Value::String(s),
        serde_json::Value::Array(arr) => {
            toml::Value::Array(arr.into_iter().map(toml_value_from_json).collect())
        }
        serde_json::Value::Object(map) => {
            let mut t = toml::value::Table::new();
            for (k, v) in map {
                t.insert(k, toml_value_from_json(v));
            }
            toml::Value::Table(t)
        }
    }
}

/// Set a value at a dotted key path, creating intermediate tables as needed.
fn set_at_path(table: &mut toml::value::Table, path: &[String], value: toml::Value) {
    if path.is_empty() {
        return;
    }
    if path.len() == 1 {
        table.insert(path[0].clone(), value);
        return;
    }
    let entry = table
        .entry(path[0].clone())
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
    if let toml::Value::Table(sub) = entry {
        set_at_path(sub, &path[1..], value);
    }
    // If the entry at this path is not a table, we can't recurse — silently skip.
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate known sections and basic structure.
fn validate(table: &toml::value::Table) -> Result<(), ConfigError> {
    // Check that known sections have the right types.
    if let Some(provider) = table.get("provider") {
        match provider {
            toml::Value::Table(tbl) => {
                if let Some(plugin) = tbl.get("plugin") {
                    if !plugin.is_str() {
                        return Err(ConfigError::Validation {
                            field: "provider.plugin".into(),
                            message: "must be a string".into(),
                        });
                    }
                }
            }
            _ => {
                return Err(ConfigError::Validation {
                    field: "provider".into(),
                    message: "must be a table".into(),
                });
            }
        }
    }

    // Check that plugin sections (plugins.*) have valid structure.
    if let Some(plugins) = table.get("plugins") {
        match plugins {
            toml::Value::Table(tbl) => {
                for (id, config) in tbl {
                    if let toml::Value::Table(pc) = config {
                        if let Some(wasm) = pc.get("wasm") {
                            if !wasm.is_str() {
                                return Err(ConfigError::Validation {
                                    field: format!("plugins.{id}.wasm"),
                                    message: "must be a string path".into(),
                                });
                            }
                        }
                    }
                }
            }
            _ => {
                return Err(ConfigError::Validation {
                    field: "plugins".into(),
                    message: "must be a table".into(),
                });
            }
        }
    }

    Ok(())
}

fn extract_plugin_sections(table: &toml::value::Table) -> HashMap<String, toml::Value> {
    let mut sections = HashMap::new();
    for (key, value) in table.iter() {
        if !KNOWN_SECTIONS.contains(&key.as_str()) {
            sections.insert(key.clone(), value.clone());
        }
    }
    sections
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ConfigError {
    Io {
        path: PathBuf,
        detail: String,
    },
    Parse {
        path: PathBuf,
        detail: String,
    },
    ImportCycle {
        path: PathBuf,
    },
    MissingEnvVar {
        name: String,
    },
    Validation {
        field: String,
        message: String,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io { path, detail } => {
                write!(f, "I/O error reading `{}`: {}", path.display(), detail)
            }
            ConfigError::Parse { path, detail } => {
                write!(f, "TOML parse error in `{}`: {}", path.display(), detail)
            }
            ConfigError::ImportCycle { path } => {
                write!(f, "import cycle detected at `{}`", path.display())
            }
            ConfigError::MissingEnvVar { name } => {
                write!(f, "required env var `${}` is not set", name)
            }
            ConfigError::Validation { field, message } => {
                write!(f, "config validation error at `{}`: {}", field, message)
            }
        }
    }
}

impl std::error::Error for ConfigError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn basic_load() {
        let dir = std::env::temp_dir().join("pan_test_basic");
        let _ = fs::create_dir_all(&dir);
        let cfg_path = dir.join("pan.toml");
        fs::write(&cfg_path, r#"
[provider]
plugin = "provider.llm"

[plugins.my_plugin]
wasm = "/tmp/test.wasm"
enabled = true
"#).unwrap();

        let cfg = Config::load(&cfg_path).unwrap();
        assert!(cfg.provider.is_some());
        assert!(cfg.plugin_sections.contains_key("my_plugin"));
        assert!(cfg.sources.contains(&cfg_path.canonicalize().unwrap()));
    }

    #[test]
    fn env_var_expansion() {
        let dir = std::env::temp_dir().join("pan_test_env");
        let _ = fs::create_dir_all(&dir);
        let cfg_path = dir.join("pan.toml");
        fs::write(&cfg_path, r#"
api_key = "${HOME}"
path = "$HOME/data"
"#).unwrap();

        let cfg = Config::load(&cfg_path).unwrap();
        let home = env::var("HOME").unwrap();
        assert_eq!(
            cfg.raw.get("api_key").and_then(|v| v.as_str()),
            Some(home.as_str())
        );
        assert_eq!(
            cfg.raw.get("path").and_then(|v| v.as_str()),
            Some(format!("{home}/data").as_str())
        );
    }

    #[test]
    fn missing_env_var_is_error() {
        let dir = std::env::temp_dir().join("pan_test_missing_env");
        let _ = fs::create_dir_all(&dir);
        let cfg_path = dir.join("pan.toml");
        fs::write(&cfg_path, r#"key = "${DOES_NOT_EXIST_XYZ123}""#).unwrap();

        let err = Config::load(&cfg_path).unwrap_err();
        assert!(matches!(err, ConfigError::MissingEnvVar { .. }));
    }

    #[test]
    fn import_resolution() {
        let dir = std::env::temp_dir().join("pan_test_imports");
        let _ = fs::create_dir_all(&dir);

        // Base config.
        fs::write(dir.join("base.toml"), r#"
[provider]
plugin = "provider.llm"
"#).unwrap();

        // Root config imports base.
        fs::write(dir.join("pan.toml"), r#"
import = ["base.toml"]

[plugins.custom]
wasm = "/tmp/custom.wasm"
"#).unwrap();

        let cfg = Config::load(dir.join("pan.toml")).unwrap();
        // Provider came from the import.
        assert!(cfg.provider.is_some());
        // Plugin came from root file.
        assert!(cfg.plugin_sections.contains_key("custom"));
    }

    #[test]
    fn import_cycle_detected() {
        let dir = std::env::temp_dir().join("pan_test_cycle");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("a.toml"), r#"import = ["b.toml"]"#).unwrap();
        fs::write(dir.join("b.toml"), r#"import = ["a.toml"]"#).unwrap();

        let err = Config::load(dir.join("a.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::ImportCycle { .. }));
    }

    #[test]
    fn validation_rejects_bad_provider_type() {
        let dir = std::env::temp_dir().join("pan_test_validate");
        let _ = fs::create_dir_all(&dir);
        let cfg_path = dir.join("pan.toml");
        fs::write(&cfg_path, r#"provider = "just-a-string""#).unwrap();

        let err = Config::load(&cfg_path).unwrap_err();
        assert!(matches!(err, ConfigError::Validation { .. }));
    }
}
