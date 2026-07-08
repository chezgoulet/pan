//! # `context.template` – prompt assembly from templates (Wave 2).
//!
//! Reads a template file and assembles the final prompt for the LLM provider.
//! The template uses `{{ var_name }}` placeholders that are substituted with
//! the current goal, context fragments, and any user-provided variables.
//!
//! Built-in variables:
//!
//! - `{{goal}}` / `{{objective}}` — the current [`Goal`]'s `objective` text.
//! - `{{context}}` — all [`Context`] fragments formatted as `[channel] body`,
//!   joined by blank lines.
//! - `{{history}}` — only those fragments whose `channel` is `"history"`,
//!   joined by blank lines (no `[history]` prefix).
//!
//! Extra variables passed via [`render`](ContextTemplate::render) override
//! built-ins, so callers can shadow any slot.
//!
//! This is the "prompt engineering" layer, separate from the raw LLM call.

use crate::registry::{Plugin, PluginError};
use crate::schema::{Context, Goal};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Template-based context assembler.
///
/// Accepts a markdown template at construction time and performs
/// `{{ var_name }}` substitution at render time to produce the assembled
/// system prompt.
#[derive(Debug, Clone)]
pub struct ContextTemplate {
    template_path: String,
    template_content: String,
}

impl ContextTemplate {
    /// Load a template from a file on disk.
    ///
    /// Returns `Err` when the file cannot be read (missing, permissions, etc.).
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .map_err(|e| format!("failed to read template {:?}: {}", path, e))?;
        Ok(Self {
            template_path: path.display().to_string(),
            template_content: content,
        })
    }

    /// Create a template from an inline string.
    ///
    /// Useful for tests and for templates that arrive as configuration values
    /// rather than filesystem paths.
    pub fn from_str(content: &str) -> Self {
        Self {
            template_path: "<inline>".into(),
            template_content: content.to_string(),
        }
    }

    /// Render the template against a goal and context, performing `{{ name }}`
    /// substitution for all built-in variables plus any caller-supplied extras.
    ///
    /// `extra_vars` keys override built-in variables — use this when a caller
    /// needs to inject values that aren't part of the core [`Goal`] or
    /// [`Context`] types (e.g. persona description, tool definitions).
    pub fn render(
        &self,
        goal: &Goal,
        ctx: &Context,
        extra_vars: &HashMap<String, String>,
    ) -> String {
        // --- Assemble built-in variable values --------

        // All context fragments, prefixed by channel.
        let context_body = ctx
            .fragments
            .iter()
            .map(|f| format!("[{}] {}", f.channel, f.body))
            .collect::<Vec<_>>()
            .join("\n\n");

        // History channel fragments only (no prefix — caller's template decides
        // how to render it).
        let history_body = ctx
            .fragments
            .iter()
            .filter(|f| f.channel == "history")
            .map(|f| f.body.clone())
            .collect::<Vec<_>>()
            .join("\n\n");

        // --- Build the variable map -------------------

        let mut vars: HashMap<String, String> = HashMap::new();
        vars.insert("context".into(), context_body);
        vars.insert("goal".into(), goal.objective.clone());
        vars.insert("objective".into(), goal.objective.clone());

        if !history_body.is_empty() {
            vars.insert("history".into(), history_body);
        }

        // Extra variables: caller-supplied values shadow built-ins.
        for (key, value) in extra_vars {
            vars.insert(key.clone(), value.clone());
        }

        // --- Substitute -------------------------------

        let mut result = self.template_content.clone();
        // Pattern is `{{key}}`.  In Rust's format! syntax, `{{` yields a
        // literal `{` and `}}` yields a literal `}`, so
        // `format!("{{{{{} }}}}", key)` → `{{key}}`.
        for (key, value) in &vars {
            let pattern = ["{{", key, "}}"].concat();
            result = result.replace(&pattern, value);
        }

        result
    }

    /// The filesystem path this template was loaded from, or `"<inline>"` if
    /// created via [`from_str`](Self::from_str).
    pub fn template_path(&self) -> &str {
        &self.template_path
    }

    /// The raw template text (before substitution). Useful for inspection.
    pub fn raw_template(&self) -> &str {
        &self.template_content
    }
}

impl Plugin for ContextTemplate {
    fn id(&self) -> &str {
        "context.template"
    }

    fn provision(&mut self) -> Result<(), PluginError> {
        // Template was already loaded during construction; if we wanted
        // deferred-loading semantics (lazy read), this is where we'd do it.
        // For now, validate that the content isn't empty as a quick sanity
        // check.
        if self.template_content.is_empty() {
            return Err(PluginError {
                plugin: self.id().to_string(),
                message: format!("template at `{}` is empty", self.template_path),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Context, Fragment, Goal, Trigger};

    fn simple_goal() -> Goal {
        Goal {
            id: "test-1".into(),
            revision: 0,
            objective: "list all files in /tmp".into(),
            trigger: Trigger::Tick { sequence: 1 },
        }
    }

    // ------------------------------------------------------------------
    // Acceptance criterion: variable substitution {{ var_name }}
    // ------------------------------------------------------------------

    #[test]
    fn substitutes_goal_variable() {
        let tmpl = ContextTemplate::from_str("You are a helpful AI.\nGoal: {{goal}}");
        let goal = simple_goal();
        let out = tmpl.render(&goal, &Context::default(), &HashMap::new());
        assert!(out.contains("list all files in /tmp"));
        assert!(!out.contains("{{goal}}"), "placeholder must be replaced");
    }

    #[test]
    fn substitutes_objective_alias() {
        let tmpl = ContextTemplate::from_str("Object: {{objective}}");
        let goal = simple_goal();
        let out = tmpl.render(&goal, &Context::default(), &HashMap::new());
        assert!(out.contains("list all files in /tmp"));
    }

    #[test]
    fn substitutes_context_fragments() {
        let tmpl = ContextTemplate::from_str("Context:\n{{context}}");
        let ctx = Context::default()
            .with("memory", "the user is Sam")
            .with("world", "it is raining");
        let out = tmpl.render(&simple_goal(), &ctx, &HashMap::new());
        assert!(out.contains("[memory]"));
        assert!(out.contains("the user is Sam"));
        assert!(out.contains("[world]"));
        assert!(out.contains("it is raining"));
    }

    #[test]
    fn substitutes_history_channel() {
        let tmpl = ContextTemplate::from_str("History:\n{{history}}");
        let ctx = Context::default()
            .with("history", "user: hi\nassistant: hello");
        let out = tmpl.render(&simple_goal(), &ctx, &HashMap::new());
        assert!(out.contains("user: hi"));
        // History should NOT get the [history] prefix — just the body.
        assert!(!out.contains("[history]"));
    }

    #[test]
    fn history_absent_when_empty() {
        let tmpl = ContextTemplate::from_str("History: {{history}}");
        let ctx = Context::default(); // no fragments at all
        let out = tmpl.render(&simple_goal(), &ctx, &HashMap::new());
        // When history body is empty, the key is not inserted, so the
        // placeholder survives.  This is intentional: the template author
        // decides what to do about it.
        assert!(out.contains("{{history}}"));
    }

    #[test]
    fn extra_vars_shadow_builtins() {
        let tmpl = ContextTemplate::from_str("Goal: {{goal}}");
        let mut extra = HashMap::new();
        extra.insert("goal".into(), "OVERRIDDEN".into());
        let out = tmpl.render(&simple_goal(), &Context::default(), &extra);
        assert!(out.contains("OVERRIDDEN"));
        assert!(!out.contains("list all files"), "built-in goal must be shadowed");
    }

    #[test]
    fn extra_vars_add_new_placeholders() {
        let tmpl = ContextTemplate::from_str("Persona: {{persona}}");
        let mut extra = HashMap::new();
        extra.insert("persona".into(), "a pirate".into());
        let out = tmpl.render(&simple_goal(), &Context::default(), &extra);
        assert!(out.contains("a pirate"));
        assert!(!out.contains("{{persona}}"));
    }

    #[test]
    fn multiple_placeholders_rendered() {
        let tmpl =
            ContextTemplate::from_str("You are {{persona}}.\nGoal: {{goal}}\nContext:\n{{context}}");
        let mut extra = HashMap::new();
        extra.insert("persona".into(), "an expert sysadmin".into());
        let ctx = Context::default().with("memory", "server is Ubuntu 24.04");
        let out = tmpl.render(&simple_goal(), &ctx, &extra);
        assert!(out.contains("expert sysadmin"));
        assert!(out.contains("list all files in /tmp"));
        assert!(out.contains("Ubuntu"));
    }

    // ------------------------------------------------------------------
    // Acceptance criterion: system prompt = template + Goal + Context
    // ------------------------------------------------------------------

    #[test]
    fn full_prompt_assembly() {
        let template = "\
# System Prompt

You are a helpful assistant.

## Current Goal
{{goal}}

## Context
{{context}}";
        let tmpl = ContextTemplate::from_str(template);
        let goal = Goal {
            id: "run-42".into(),
            revision: 0,
            objective: "check disk usage".into(),
            trigger: Trigger::Tick { sequence: 42 },
        };
        let ctx = Context::default()
            .with("memory", "server is at /dev/sda1")
            .with("history", "user: check disk\nassistant: running df -h");

        let out = tmpl.render(&goal, &ctx, &HashMap::new());

        // The output should be the template structure with values injected.
        assert!(out.starts_with("# System Prompt"), "template structure preserved");
        assert!(out.contains("check disk usage"), "goal objective injected");
        assert!(out.contains("server is at /dev/sda1"), "context injected");
        assert!(out.contains("user: check disk"), "history in context");
    }

    // ------------------------------------------------------------------
    // File-backed template
    // ------------------------------------------------------------------

    #[test]
    fn loads_from_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("pan_test_template.md");
        let content = "Prefix: {{goal}} / {{context}}";
        std::fs::write(&path, content).expect("write test template file");

        let tmpl = ContextTemplate::new(&path).expect("load template from file");
        let out = tmpl.render(&simple_goal(), &Context::default(), &HashMap::new());
        assert!(out.contains("Prefix:"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn file_not_found_returns_error() {
        let err = ContextTemplate::new("/nonexistent/template.md").unwrap_err();
        assert!(err.contains("failed to read template"));
    }

    #[test]
    fn empty_file_fails_provision() {
        let dir = std::env::temp_dir();
        let path = dir.join("pan_test_empty.md");
        std::fs::write(&path, "").expect("write empty template file");

        let mut tmpl = ContextTemplate::new(&path).expect("load empty file");
        let err = tmpl.provision().unwrap_err();
        assert!(err.message.contains("empty"));

        std::fs::remove_file(&path).ok();
    }

    // ------------------------------------------------------------------
    // Identity & lifecycle
    // ------------------------------------------------------------------

    #[test]
    fn plugin_id_is_context_dot_template() {
        let tmpl = ContextTemplate::from_str("hello");
        assert_eq!(tmpl.id(), "context.template");
    }

    #[test]
    fn from_str_marks_path_as_inline() {
        let tmpl = ContextTemplate::from_str("hello");
        assert_eq!(tmpl.template_path(), "<inline>");
    }

    #[test]
    fn raw_template_returns_unsubstituted_content() {
        let tmpl = ContextTemplate::from_str("{{goal}} raw");
        assert_eq!(tmpl.raw_template(), "{{goal}} raw");
    }
}
