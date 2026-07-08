//! # `skill.runner` — agentskills.io polyglot skill execution (Ring 2).
//!
//! Scans a directory for markdown skill files, parses them into capabilities,
//! and registers each as a discoverable, invocable capability. Polyglot: any
//! installed interpreter (shell, Python, Node, Ruby, …).
//!
//! ## Skill format (agentskills.io)
//!
//! ```markdown
//! ---
//! name: greet
//! description: Say hello to someone
//! language: shell
//! ---
//!
//! ```sh
//! echo "Hello, $SKILL_ARG_NAME!"
//! ```
//! ```
//!
//! The executable section is the first fenced code block if one is present, or
//! the entire body after the frontmatter if not.
//!
//! ## Architecture
//!
//! The runner is a lifecycle [`Plugin`] that:
//!
//! 1. Scans `skill_dir` for `*.md` files at provision time.
//! 2. Parses each file into a [`SkillFile`].
//! 3. Makes discovered skills available via [`Self::skills()`].
//!
//! The host (CLI, daemon, test) calls [`Self::register_with()`] to wire each
//! skill as a [`Capability`] + executor handler.
//!
//! ## Polyglot execution
//!
//! The `language` field in frontmatter selects the interpreter:
//!
//! | language string | interpreter |
//! |---|---|
//! | `shell`, `sh` | `sh -c` |
//! | `bash` | `bash -c` |
//! | `python`, `py`, `python3` | `python3 -c` |
//! | `node`, `javascript`, `js` | `node -e` |
//! | `ruby`, `rb` | `ruby -e` |
//! | `perl`, `pl` | `perl -e` |
//!
//! Unknown languages fall back to `sh -c` with a warning. Rust (`rs`) is
//! explicitly refused until compilation support lands (W4–7).
//!
//! ## Sandboxing
//!
//! Sandboxed execution via `exec.docker` is deferred to W4–7 (issue #45
//! dependencies). Wave 2 runs skills on the host, matching the same
//! unsandboxed stance as `exec.local` — the govern stage is the sole gate.

use crate::pipeline::ExecError;
use crate::schema::{Capability, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The `skill.runner` plugin. Scans a directory for skill files at provision
/// time and makes them available as invocable capabilities.
pub struct SkillRunner {
    /// Directory to scan for skill files (`*.md`).
    skill_dir: PathBuf,
    /// Discovered skills, populated by `provision()`.
    skills: Vec<SkillFile>,
}

impl SkillRunner {
    /// Create a new runner targeting `skill_dir`.
    ///
    /// Scanning happens lazily at [`provision()`](Self::provision) time or via
    /// an explicit [`scan()`](Self::scan) call — the constructor itself is cheap.
    pub fn new(skill_dir: impl Into<PathBuf>) -> Self {
        Self {
            skill_dir: skill_dir.into(),
            skills: Vec::new(),
        }
    }

    /// Scan the skill directory and parse all valid `*.md` skill files.
    ///
    /// A non-existent or empty directory produces an empty list (not an error),
    /// so the runner is safe to configure with a path that doesn't exist yet.
    /// Unparseable or nameless files are skipped with a warning to stderr.
    pub fn scan(&self) -> Result<Vec<SkillFile>, ScanError> {
        let mut skills = Vec::new();

        let dir = match std::fs::read_dir(&self.skill_dir) {
            Ok(d) => d,
            Err(e) => {
                return if self.skill_dir.exists() {
                    Err(ScanError(format!(
                        "read skill dir {}: {e}",
                        self.skill_dir.display()
                    )))
                } else {
                    // Non-existent dir = no skills, not an error.
                    Ok(Vec::new())
                };
            }
        };

        for entry in dir {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("[skill.runner] skipping directory entry: {e}");
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[skill.runner] skip unreadable {}: {e}", path.display());
                    continue;
                }
            };
            match SkillFile::parse(&text) {
                Ok(skill) => {
                    if skill.name.is_empty() {
                        eprintln!(
                            "[skill.runner] skip {}: no name in frontmatter",
                            path.display()
                        );
                        continue;
                    }
                    skills.push(skill);
                }
                Err(e) => {
                    eprintln!("[skill.runner] skip {}: {e}", path.display());
                }
            }
        }
        Ok(skills)
    }

    /// The discovered skills (populated after [`provision()`](Self::provision)).
    pub fn skills(&self) -> &[SkillFile] {
        &self.skills
    }

    /// Register each discovered skill as a capability in `registry` and wire
    /// its handler via the `register_handler` closure (e.g. onto
    /// [`LocalExecutor`](crate::plugins::exec_local::LocalExecutor)).
    ///
    /// The closure receives `(capability_id, handler)` and should register the
    /// handler under that id. Each handler, when called, runs the skill's code
    /// through the appropriate interpreter with the given args.
    ///
    /// Skills whose names collide with existing capabilities are silently
    /// skipped (the registry's conflict error is ignored — the first
    /// registration wins), matching the core's never-last-wins stance.
    pub fn register_with<F>(
        &self,
        registry: &mut crate::registry::CapabilityRegistry,
        register_handler: F,
    ) where
        F: Fn(&str, Box<dyn Fn(&Value) -> Result<Value, ExecError> + Send + Sync>),
    {
        for skill in &self.skills {
            let cap_id = format!("skill.{}", skill.name);
            // Register the capability so the provider can see it.
            let _ = registry.register(Capability::new(cap_id.clone(), skill.description.clone(), skill.args_schema()));
            // Register the handler that executes the skill code.
            let code = skill.code.clone();
            let lang = skill.language.clone();
            register_handler(&cap_id, Box::new(move |args| run_skill(&lang, &code, args)));
        }
    }
}

impl crate::registry::Plugin for SkillRunner {
    fn id(&self) -> &str {
        "skill.runner"
    }

    fn provision(&mut self) -> Result<(), crate::registry::PluginError> {
        match self.scan() {
            Ok(skills) => {
                let count = skills.len();
                self.skills = skills;
                eprintln!(
                    "[skill.runner] discovered {count} skill(s) in {}",
                    self.skill_dir.display()
                );
                Ok(())
            }
            Err(e) => Err(crate::registry::PluginError {
                plugin: self.id().to_string(),
                message: format!("scan failed: {e}"),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Skill file format
// ---------------------------------------------------------------------------

/// A parsed skill file. Skills define an executable capability with metadata.
#[derive(Debug, Clone)]
pub struct SkillFile {
    /// Short identifier, used as the capability name (`skill.<name>`).
    pub name: String,
    /// Human-readable description shown to the provider.
    pub description: String,
    /// Interpreter language: `"shell"`, `"python"`, `"node"`, `"ruby"`, etc.
    pub language: String,
    /// Declared arguments, parsed from frontmatter `args:` block.
    pub args: Vec<SkillArg>,
    /// The executable code body (from code block or body text).
    pub code: String,
}

/// One named argument a skill accepts.
#[derive(Debug, Clone)]
pub struct SkillArg {
    pub name: String,
    pub type_hint: String,
    pub description: String,
}

impl SkillFile {
    /// Parse a skill from markdown text.
    ///
    /// ## Format
    ///
    /// ```text
    /// ---
    /// name: my-skill
    /// description: Does a thing
    /// language: shell
    /// ---
    ///
    /// ```sh
    /// echo "hello"
    /// ```
    /// ```
    ///
    /// The frontmatter block (between `---` delimiters) is optional. If absent,
    /// the entire text is treated as body, and the first fenced code block
    /// (or body if no fence) is the executable section.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let text = text.trim();

        // Split frontmatter from body at --- delimiters.
        let (frontmatter, body) = if text.starts_with("---") {
            let rest = &text[3..];
            let end = rest
                .find("\n---")
                .ok_or_else(|| ParseError("unclosed frontmatter (no closing `---`)".into()))?;
            let fm = &rest[..end];
            let body_start = end + 4; // skip `\n---` and possibly a trailing `\n`
            (Some(fm), rest[body_start..].trim())
        } else {
            (None, text)
        };

        let mut name = String::new();
        let mut description = String::new();
        let mut language = String::from("shell");
        let mut args: Vec<SkillArg> = Vec::new();
        let mut in_args_section = false;

        // Lightweight frontmatter parser (YAML subset).
        if let Some(fm) = frontmatter {
            for line in fm.lines() {
                let trimmed = line.trim();

                // Check for top-level keys.
                if let Some(val) = trimmed.strip_prefix("name:") {
                    name = val.trim().to_string();
                    in_args_section = false;
                } else if let Some(val) = trimmed.strip_prefix("description:") {
                    description = val.trim().to_string();
                    in_args_section = false;
                } else if let Some(val) = trimmed.strip_prefix("language:") {
                    language = val.trim().to_string();
                    in_args_section = false;
                } else if trimmed == "args:" || trimmed.starts_with("args:") {
                    in_args_section = true;
                } else if in_args_section {
                    // Inside args block: expect `  arg-name:` or `    type:`
                    if trimmed.starts_with('-') {
                        // List item: `- arg-name` or `- arg-name:`
                        let arg_name = trimmed
                            .strip_prefix('-')
                            .unwrap_or("")
                            .trim()
                            .trim_end_matches(':')
                            .to_string();
                        if !arg_name.is_empty() {
                            args.push(SkillArg {
                                name: arg_name,
                                type_hint: String::from("string"),
                                description: String::new(),
                            });
                        }
                    } else if let Some((sub_key, sub_val)) = trimmed.split_once(':') {
                        // Nested key: `type: string` or `description: ...`
                        let sk = sub_key.trim();
                        let sv = sub_val.trim();
                        if sk == "type" || sk == "description" {
                            if let Some(last) = args.last_mut() {
                                if sk == "type" {
                                    last.type_hint = sv.to_string();
                                } else {
                                    last.description = sv.to_string();
                                }
                            }
                        }
                    }
                }
            }
        }

        // Extract the executable code.
        let code = if body.contains("```") {
            extract_first_code_block(body).unwrap_or_default()
        } else {
            // No fenced block: treat the entire body as code.
            body.to_string()
        };

        Ok(SkillFile {
            name,
            description,
            language,
            args,
            code: code.trim().to_string(),
        })
    }

    /// Generate a JSON Schema for the args this skill expects, suitable for
    /// use as a [`Capability::args_schema`].
    pub fn args_schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        for arg in &self.args {
            let schema_type = match arg.type_hint.as_str() {
                "string" => "string",
                "number" => "number",
                "integer" | "int" => "integer",
                "boolean" | "bool" => "boolean",
                "array" => "array",
                "object" => "object",
                _ => "string",
            };
            let prop = serde_json::json!({
                "type": schema_type,
                "description": arg.description,
            });
            properties.insert(arg.name.clone(), prop);
        }
        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": [],
        })
    }
}

/// Extract the first fenced code block from markdown text, returning its
/// inner content (without the fence lines).
fn extract_first_code_block(text: &str) -> Option<String> {
    let mut lines = text.lines().peekable();

    // Find the opening fence: a line starting with ``` (3+ backticks).
    let opening = lines.find(|l| {
        let t = l.trim_start();
        t.starts_with("```")
    })?;

    let fence_prefix = opening.trim_start();
    let fence_len = fence_prefix.chars().take_while(|&c| c == '`').count();

    let mut code_lines: Vec<&str> = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        // Check for closing fence (same or more backticks).
        let backtick_count = trimmed.chars().take_while(|&c| c == '`').count();
        if backtick_count >= fence_len && trimmed[backtick_count..].trim().is_empty() {
            break;
        }
        code_lines.push(line);
    }

    if code_lines.is_empty() {
        None
    } else {
        Some(code_lines.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// Polyglot execution
// ---------------------------------------------------------------------------

/// Run a skill's code with the appropriate interpreter.
///
/// Args are passed as environment variables prefixed `SKILL_ARG_` (uppercased
/// key names), so the skill can access them regardless of language:
///
/// - Shell: `$SKILL_ARG_NAME`
/// - Python: `os.environ["SKILL_ARG_NAME"]`
/// - Node: `process.env.SKILL_ARG_NAME`
pub fn run_skill(language: &str, code: &str, args: &Value) -> Result<Value, ExecError> {
    let code = code.trim();
    if code.is_empty() {
        return Err(ExecError(
            "skill.runner: skill has no executable code".into(),
        ));
    }

    let interpreter = resolve_interpreter(language)?;
    let mut cmd = Command::new(&interpreter[0]);
    for arg in &interpreter[1..] {
        cmd.arg(arg);
    }
    cmd.arg(code);

    // Set environment variables from args (uppercased, SKILL_ARG_ prefix).
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            let val = match v {
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            cmd.env(format!("SKILL_ARG_{}", k.to_uppercase()), val);
        }
    }

    match cmd.output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            Ok(serde_json::json!({
                "exit_code": out.status.code(),
                "stdout": stdout,
                "stderr": stderr,
                "success": out.status.success(),
            }))
        }
        Err(e) => Err(ExecError(format!("skill.runner failed to spawn: {e}"))),
    }
}

/// Map a language name to the interpreter command and its code-passing flag.
fn resolve_interpreter(language: &str) -> Result<Vec<String>, ExecError> {
    match language.trim().to_lowercase().as_str() {
        "shell" | "sh" => Ok(vec!["sh".into(), "-c".into()]),
        "bash" => Ok(vec!["bash".into(), "-c".into()]),
        "python" | "py" | "python3" => Ok(vec!["python3".into(), "-c".into()]),
        "node" | "javascript" | "js" => Ok(vec!["node".into(), "-e".into()]),
        "ruby" | "rb" => Ok(vec!["ruby".into(), "-e".into()]),
        "perl" | "pl" => Ok(vec!["perl".into(), "-e".into()]),
        "rust" | "rs" => Err(ExecError(
            "skill.runner: Rust (compiled) skills are not supported yet \
             (W4–7 will add compilation sandboxing)"
                .into(),
        )),
        other => {
            // Unknown language: warn and fall back to `sh -c`.
            eprintln!("[skill.runner] unknown language `{other}`, falling back to `sh -c`");
            Ok(vec!["sh".into(), "-c".into()])
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A skill file could not be parsed.
#[derive(Debug, Clone)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "skill parse: {}", self.0)
    }
}
impl std::error::Error for ParseError {}

/// A scan of the skill directory failed.
#[derive(Debug, Clone)]
pub struct ScanError(pub String);

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "skill scan: {}", self.0)
    }
}
impl std::error::Error for ScanError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{CapabilityRegistry, Plugin};

    // ── Skill parsing ──────────────────────────────────────────────────────

    #[test]
    fn parse_simple_shell_skill() {
        let md = r#"---
name: greet
description: Say hello
language: shell
---

```sh
echo "Hello, World!"
```
"#;
        let skill = SkillFile::parse(md).unwrap();
        assert_eq!(skill.name, "greet");
        assert_eq!(skill.description, "Say hello");
        assert_eq!(skill.language, "shell");
        assert!(skill.code.contains("Hello, World!"));
    }

    #[test]
    fn parse_skill_with_args() {
        let md = r#"---
name: list-files
description: List files in a directory
language: shell
args:
  - path
    type: string
    description: Directory to list
---

```sh
ls -la /tmp
```
"#;
        let skill = SkillFile::parse(md).unwrap();
        assert_eq!(skill.name, "list-files");
        assert_eq!(skill.args.len(), 1);
        assert_eq!(skill.args[0].name, "path");
    }

    #[test]
    fn parse_skill_without_frontmatter() {
        let md = "\n```sh\necho \"hi\"\n```\n";
        let skill = SkillFile::parse(md).unwrap();
        assert!(skill.name.is_empty(), "no name without frontmatter");
        assert_eq!(skill.language, "shell");
        assert!(skill.code.contains("echo"));
    }

    #[test]
    fn parse_skill_without_code_block_uses_body() {
        let md = "# greet\necho hello";
        let skill = SkillFile::parse(md).unwrap();
        // Without a fenced code block, the entire body is treated as code.
        assert_eq!(skill.code, "# greet\necho hello");
    }

    #[test]
    fn parse_skill_frontmatter_only_no_code() {
        let md = r#"---
name: empty
description: No code
language: shell
---
"#;
        let skill = SkillFile::parse(md).unwrap();
        assert!(skill.code.is_empty());
    }

    #[test]
    fn parse_rejects_unclosed_frontmatter() {
        let md = "---\nname: bad";
        let result = SkillFile::parse(md);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unclosed"));
    }

    #[test]
    fn parse_skill_args_schema_generation() {
        let skill = SkillFile {
            name: "test".into(),
            description: "".into(),
            language: "shell".into(),
            args: vec![
                SkillArg {
                    name: "path".into(),
                    type_hint: "string".into(),
                    description: "File path".into(),
                },
                SkillArg {
                    name: "count".into(),
                    type_hint: "integer".into(),
                    description: "Count".into(),
                },
            ],
            code: "echo $SKILL_ARG_PATH".into(),
        };
        let schema = skill.args_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["path"]["type"].as_str().unwrap() == "string");
        assert!(schema["properties"]["count"]["type"].as_str().unwrap() == "integer");
    }

    // ── Runner scanning ────────────────────────────────────────────────────

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pan_skill_test_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn runner_discovers_skills_in_dir() {
        let dir = temp_dir("discover");
        std::fs::write(
            dir.join("greet.md"),
            r#"---
name: greet
description: Say hello
language: shell
---

```sh
echo "Hello!"
```
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("ping.md"),
            r#"---
name: ping
description: Ping a host
language: sh
---

```sh
ping -c 1 localhost
```
"#,
        )
        .unwrap();
        // A non-skill file should be ignored.
        std::fs::write(dir.join("readme.txt"), "not a skill").unwrap();

        let runner = SkillRunner::new(&dir);
        let skills = runner.scan().unwrap();
        assert_eq!(skills.len(), 2);
        assert!(skills.iter().any(|s| s.name == "greet"));
        assert!(skills.iter().any(|s| s.name == "ping"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runner_ignores_missing_directory() {
        let runner = SkillRunner::new("/tmp/pan_skill_test_nonexistent_xyzzy");
        let skills = runner.scan().unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn runner_registers_capabilities() {
        let dir = temp_dir("register");
        std::fs::write(
            dir.join("greet.md"),
            r#"---
name: greet
description: Say hello
language: shell
---
```sh
echo "Hello!"
```
"#,
        )
        .unwrap();

        let mut runner = SkillRunner::new(&dir);
        runner.provision().unwrap();
        assert_eq!(runner.skills().len(), 1);

        let mut registry = CapabilityRegistry::new();
        let mut registered_caps: Vec<String> = Vec::new();
        runner.register_with(&mut registry, |cap_id, _handler| {
            registered_caps.push(cap_id.to_string());
        });

        let cap = registry.lookup("skill.greet");
        assert!(
            cap.is_some(),
            "skill.greet must be registered as capability"
        );
        assert_eq!(cap.unwrap().summary, "Say hello");
        assert!(registered_caps.contains(&"skill.greet".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_dir_scans_to_empty() {
        let dir = temp_dir("empty");
        let runner = SkillRunner::new(&dir);
        let skills = runner.scan().unwrap();
        assert!(skills.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skip_nameless_skills() {
        let dir = temp_dir("nameless");
        std::fs::write(
            dir.join("nameless.md"),
            "---\ndescription: No name\nlanguage: sh\n---\n```sh\necho hi\n```",
        )
        .unwrap();
        let runner = SkillRunner::new(&dir);
        let skills = runner.scan().unwrap();
        assert!(skills.is_empty(), "nameless skills must be skipped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Polyglot execution ─────────────────────────────────────────────────

    #[test]
    fn shell_skill_runs() {
        let result = run_skill("shell", "echo hello-skill-runner", &Value::Null).unwrap();
        assert_eq!(
            result["stdout"].as_str().unwrap().trim(),
            "hello-skill-runner"
        );
        assert_eq!(result["exit_code"], 0);
        assert_eq!(result["success"], true);
    }

    #[test]
    fn python_skill_runs() {
        let result = run_skill("python", "print('hello from python')", &Value::Null).unwrap();
        let out = result["stdout"].as_str().unwrap().trim();
        assert_eq!(out, "hello from python");
    }

    #[test]
    fn bash_skill_runs() {
        let code = "name='Bash'; echo \"Hello from $name\"";
        let result = run_skill("bash", code, &Value::Null).unwrap();
        assert_eq!(result["stdout"].as_str().unwrap().trim(), "Hello from Bash");
    }

    #[test]
    fn skill_args_become_env_vars() {
        let code = "echo \"Hello, $SKILL_ARG_NAME!\"";
        let args = serde_json::json!({"name": "World"});
        let result = run_skill("shell", code, &args).unwrap();
        assert!(result["stdout"].as_str().unwrap().contains("World"));
    }

    #[test]
    fn python_skill_receives_args_via_env() {
        let code = "import os; print(f\"Hello, {os.environ['SKILL_ARG_NAME']}!\")";
        let args = serde_json::json!({"name": "Pan"});
        let result = run_skill("python", code, &args).unwrap();
        assert_eq!(result["stdout"].as_str().unwrap().trim(), "Hello, Pan!");
    }

    #[test]
    fn node_skill_runs() {
        let code = "console.log('hello from node')";
        let result = run_skill("node", code, &Value::Null);
        match result {
            Ok(v) => assert!(v["stdout"].as_str().unwrap_or("").contains("hello")),
            Err(_) => {
                // Node may not be installed in CI; skip gracefully.
                eprintln!("node not available, skipping test");
            }
        }
    }

    #[test]
    fn empty_code_errors() {
        let result = run_skill("shell", "", &Value::Null);
        assert!(result.is_err());
        assert!(result.unwrap_err().0.contains("no executable code"));
    }

    #[test]
    fn rust_skills_refused() {
        let result = run_skill("rust", "fn main() {}", &Value::Null);
        assert!(result.is_err());
        assert!(result.unwrap_err().0.contains("not supported"));
    }

    #[test]
    fn unknown_language_falls_back_to_shell() {
        let result = run_skill("foobar", "echo fallback-ok", &Value::Null).unwrap();
        assert_eq!(result["stdout"].as_str().unwrap().trim(), "fallback-ok");
    }

    #[test]
    fn skill_reports_failure_exit_code() {
        let result = run_skill("shell", "exit 42", &Value::Null).unwrap();
        assert_eq!(result["exit_code"], 42);
        assert_eq!(result["success"], false);
    }

    #[test]
    fn skill_captures_stderr() {
        let result = run_skill("shell", "echo out; echo err >&2", &Value::Null).unwrap();
        assert!(result["stdout"].as_str().unwrap().contains("out"));
        assert!(result["stderr"].as_str().unwrap().contains("err"));
    }

    // ── End-to-end: lifecycle + execution ──────────────────────────────────

    #[test]
    fn discovered_skill_is_fully_runnable() {
        let dir = temp_dir("e2e");
        std::fs::write(
            dir.join("hello.md"),
            r#"---
name: hello
description: Print a greeting
language: shell
args:
  - name
    type: string
    description: Who to greet
---

```sh
echo "Hello, $SKILL_ARG_NAME!"
```
"#,
        )
        .unwrap();

        let mut runner = SkillRunner::new(&dir);
        runner.provision().unwrap();

        // Simulate what the CLI does: register capabilities + handler.
        let mut registry = CapabilityRegistry::new();
        let mut last_result: Option<Value> = None;
        runner.register_with(&mut registry, |cap_id, handler| {
            if cap_id == "skill.hello" {
                // Execute the skill with args.
                let args = serde_json::json!({"name": "Pan"});
                last_result = Some(handler(&args).unwrap());
            }
        });

        let cap = registry.lookup("skill.hello");
        assert!(cap.is_some());

        let result = last_result.expect("handler must have been called");
        assert!(result["stdout"].as_str().unwrap().contains("Pan"));
        assert_eq!(result["exit_code"], 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_lifecycle_integration() {
        let dir = temp_dir("lifecycle");
        std::fs::write(
            dir.join("ping.md"),
            r#"---
name: ping
description: Ping check
language: sh
---
```sh
echo pong
```
"#,
        )
        .unwrap();

        let mut lifecycle = crate::registry::Lifecycle::new();
        lifecycle.register(Box::new(SkillRunner::new(&dir)));
        lifecycle.provision().unwrap();
        lifecycle.validate().unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }
}
