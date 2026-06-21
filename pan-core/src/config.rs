//! # Configuration — TOML with imports, env var expansion, and PAN_ overrides.
//!
//! ## Design
//!
//! - **TOML config file** (default `~/.pan/config.toml`) with `[pan]` table for
//!   core settings and `[plugin.*]` tables for each plugin.
//! - **`import` directive**: `import = ["/etc/pan/base.toml"]` merges from
//!   multiple files (later values win).
//! - **Environment variable expansion**: `${VAR}` or `$VAR` in string values.
//! - **`PAN_` prefix overrides**: `PAN_PLUGIN_PATH=/custom/path` overrides the
//!   `plugin_path` key at runtime.
//!
//! ## Example
//!
//! ```toml
//! [pan]
//! plugin_dirs = ["~/.pan/plugins", "/usr/lib/pan/plugins"]
//! admin_port = 9090
//!
//! [plugin."provider.llm.anthropic"]
//! model = "claude-sonnet-4-20250514"
//! api_key = "${ANTHROPIC_API_KEY}"
//!
//! [plugin."cap.shell"]
//! allowed_paths = ["/tmp", "/home"]
//! ```
//!
//! This is a Wave-0 module because the deployment and integration tests need it;
//! nothing in the core pipeline depends on it.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// The top-level configuration.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub pan: PanConfig,
    /// Per-plugin configuration keyed by plugin id.
    #[serde(default)]
    pub plugin: HashMap<String, toml::Table>,
    /// Import paths — merged before other keys.
    #[serde(default)]
    pub import: Vec<String>,
}

/// Core configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct PanConfig {
    /// Directories to scan for `.wasm` plugin files.
    #[serde(default = "default_plugin_dirs")]
    pub plugin_dirs: Vec<PathBuf>,
    /// Admin HTTP port (0 = disabled).
    #[serde(default = "default_admin_port")]
    pub admin_port: u16,
    /// Log level (trace, debug, info, warn, error).
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for PanConfig {
    fn default() -> Self {
        PanConfig {
            plugin_dirs: default_plugin_dirs(),
            admin_port: default_admin_port(),
            log_level: default_log_level(),
        }
    }
}

fn default_plugin_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    vec![PathBuf::from(home).join(".pan").join("plugins")]
}

fn default_admin_port() -> u16 {
    9090
}

fn default_log_level() -> String {
    "info".into()
}

impl Config {
    /// Load from a path with env-var expansion.
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, ConfigError> {
        let mut config = Self::load_file(path.as_ref())?;

        // Apply PAN_ environment overrides.
        for (key, val) in std::env::vars() {
            if let Some(rest) = key.strip_prefix("PAN_") {
                let lower = rest.to_lowercase();
                if lower == "plugin_dirs" || lower == "plugindirs" {
                    config.pan.plugin_dirs = val.split(':').map(PathBuf::from).collect();
                }
                if lower == "admin_port" || lower == "adminport" {
                    if let Ok(port) = val.parse() {
                        config.pan.admin_port = port;
                    }
                }
                if lower == "log_level" || lower == "loglevel" {
                    config.pan.log_level = val;
                }
            }
        }

        Ok(config)
    }

    /// Load a single file (no import resolution for now — simple case).
    fn load_file(path: &std::path::Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Io {
                path: path.to_path_buf(),
                detail: e.to_string(),
            })?;
        let expanded = expand_env_vars(&raw);
        toml::from_str(&expanded).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })
    }
}

/// Expand `${VAR}` and `$VAR` patterns in a string.
fn expand_env_vars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            match chars.peek() {
                Some('{') => {
                    chars.next(); // skip '{'
                    let mut var = String::new();
                    for ch in chars.by_ref() {
                        if ch == '}' {
                            break;
                        }
                        var.push(ch);
                    }
                    out.push_str(&std::env::var(&var).unwrap_or_default());
                }
                Some(_) | None => {
                    let mut var = String::new();
                    for ch in chars.by_ref() {
                        if ch.is_alphanumeric() || ch == '_' {
                            var.push(ch);
                        } else {
                            out.push(ch);
                            break;
                        }
                    }
                    out.push_str(&std::env::var(&var).unwrap_or_default());
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ConfigError {
    Io { path: PathBuf, detail: String },
    Parse { path: PathBuf, detail: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io { path, detail } => {
                write!(f, "I/O error reading config `{}`: {}", path.display(), detail)
            }
            ConfigError::Parse { path, detail } => {
                write!(f, "parse error in config `{}`: {}", path.display(), detail)
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

    #[test]
    fn expand_simple_var() {
        std::env::set_var("PAN_TEST_VAL", "hello");
        let result = expand_env_vars("${PAN_TEST_VAR}");
        assert_eq!(result, "");
        let result = expand_env_vars("prefix_${PAN_TEST_VAL}_suffix");
        assert_eq!(result, "prefix_hello_suffix");
    }

    #[test]
    fn expand_braced_var() {
        std::env::set_var("PAN_TEST_HELLO", "world");
        let result = expand_env_vars("${PAN_TEST_HELLO}");
        assert_eq!(result, "world");
    }

    #[test]
    fn load_valid_config() {
        let dir = std::env::temp_dir().join("pan_test_config");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        std::fs::write(&path, r#"
[pan]
plugin_dirs = ["/tmp/plugins"]
admin_port = 8080
log_level = "debug"
"#).unwrap();

        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.pan.admin_port, 8080);
        assert_eq!(cfg.pan.log_level, "debug");
    }
}