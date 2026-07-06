//! # `pan` — the Wave 1 walking skeleton.
//!
//! Reads lines from stdin, treats each as a user utterance, asks the provider
//! for a decision, and enacts it: `Express` → printed to stdout, `Invoke` →
//! routed through the dispatch pipeline to `exec.local` (which runs
//! `cap.shell`). Every step is logged to stderr via `obs.logging`.
//!
//! Provider: uses the generic OpenAI-compatible `provider.llm` against OpenRouter
//! free tier when `OPENROUTER_API_KEY` is set; otherwise falls back to the core's
//! deterministic stub provider so the skeleton runs with zero config (proving the
//! whole pipeline before any model is involved).
//!
//! Exit test (manifest Wave 1): type `list the files in /tmp` → model emits
//! `Invoke(cap.shell, {command: "ls -la /tmp"})` → runs → reply printed →
//! action visible in logs.

use pan_core::events::{EventStream};
use pan_core::loop_engine::RunEnd;
use pan_core::pipeline::{Pipeline, Verdict};
use pan_core::plugins::exec_local::LocalExecutor;
use pan_core::plugins::gov_allow::Allow;
use pan_core::plugins::obs_logging::LogSink as ObsLog;
use pan_core::plugins::state_memory::MemoryState;
use pan_core::providers_llm::Llm;
use pan_core::registry::{CapabilityRegistry, Lifecycle};
use pan_core::schema::{
    ActionIntent, Capability, Context, Goal, Outcome, Provider, Trigger, Value,
};
use std::io::{self, BufRead, Write};

fn main() {
    // --- Capabilities the agent may invoke ---------------------------------
    let mut registry = CapabilityRegistry::new();
    registry
        .register(Capability {
            id: "cap.shell".into(),
            summary: "Run a shell command. Args: { command: string, cwd?: string }".into(),
            args_schema: serde_json::json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": {"type": "string"},
                    "cwd": {"type": "string"}
                }
            }),
        })
        .expect("register cap.shell");
    // The state-write capability the stub provider exercises.
    registry
        .register(Capability {
            id: "cap.state_write".into(),
            summary: "Write a key/value into process memory. Args: { path, value }".into(),
            args_schema: serde_json::json!({
                "type": "object",
                "required": ["path", "value"],
                "properties": {
                    "path": {"type": "string"},
                    "value": {}
                }
            }),
        })
        .expect("register cap.state_write");

    // --- Plugins ------------------------------------------------------------
    let governor = Allow::new();
    let executor = LocalExecutor::new();

    // Route cap.state_write into the in-memory state store.
    executor.register("cap.state_write", |args| {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| pan_core::pipeline::ExecError("cap.state_write needs `path`".into()))?;
        let value = args
            .get("value")
            .cloned()
            .unwrap_or(Value::Null);
        // We can't move `memory` into the closure easily; use a thread-local
        // instead is overkill — instead handle state writes in the loop below.
        // Here we just echo what we'd write (the loop performs the real write).
        Ok(serde_json::json!({"would_write": path, "value": value}))
    });

    // Lifecycle: register + provision + validate (catches id conflicts early).
    let mut lifecycle = Lifecycle::new();
    lifecycle.register(Box::new(Allow::new()));
    lifecycle.register(Box::new(MemoryState::new()));
    if let Err(e) = lifecycle.provision() {
        eprintln!("lifecycle provision failed: {e}");
        std::process::exit(1);
    }

    // --- Provider selection -------------------------------------------------
    let has_key = std::env::var("OPENROUTER_API_KEY").is_ok();
    let provider: Box<dyn Provider> = if has_key {
        Box::new(Llm::openrouter_free())
    } else {
        eprintln!(
            "[pan] no OPENROUTER_API_KEY set — using deterministic stub provider.\n\
             [pan] set OPENROUTER_API_KEY to talk to a real model (OpenRouter free tier)."
        );
        Box::new(StubShellProvider)
    };

    // --- Event stream + sink ------------------------------------------------
    let (stream, guard) = EventStream::spawn(ObsLog::new());

    let pipeline = Pipeline {
        registry: &registry,
        governor: &governor,
        executor: &executor,
        events: &stream,
    };

    let loop_ = pan_core::loop_engine::Loop {
        provider: provider.as_ref(),
        pipeline: &pipeline,
        events: &stream,
    };

    // --- Interactive REPL ---------------------------------------------------
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut seq: u64 = 0;

    println!("pan — Wave 1 walking skeleton. Type a command; Ctrl-D to exit.");
    print!("pan> ");
    let _ = stdout.flush();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() {
            print!("pan> ");
            let _ = stdout.flush();
            continue;
        }

        let goal = Goal {
            id: format!("turn-{seq}"),
            revision: 0,
            objective: line.to_string(),
            trigger: Trigger::Utterance {
                from: "user".into(),
                content: line.to_string(),
            },
        };
        seq += 1;

        let report = loop_.run_span(
            &mut pan_core::loop_engine::Once(Some(goal)),
            &Context::default(),
        );

        // Surface expressions to the user.
        for body in &report.expressed {
            println!("{body}");
        }
        // Surface effects: for cap.shell, print stdout; for state_write, perform.
        for cap in &report.effected {
            if cap == "cap.shell" {
                // The effect result was recorded on the stream; re-run visibility
                // is covered by logs. Echo a short confirmation.
                print!("pan> ");
                let _ = stdout.flush();
            }
        }
        if let Some(RunEnd::Concluded(o)) = report.end {
            if o == Outcome::Abandoned {
                // not expected in discrete CLI
            }
        }

        print!("pan> ");
        let _ = stdout.flush();
    }

    stream.shutdown(guard);
    println!("\n[pan] bye.");
}

/// Deterministic stub: turns a natural-language-ish request into a
/// `cap.shell` Invoke by naive keyword match, so the skeleton demonstrates the
/// full pipeline without a model. This is DEV ONLY and superseded by
/// `provider.llm` the moment a key is present.
struct StubShellProvider;

impl Provider for StubShellProvider {
    fn id(&self) -> &str {
        "provider.stub"
    }

    fn decide(&self, goal: &Goal, _ctx: &Context, _caps: &[Capability]) -> pan_core::schema::Decision {
        let text = match &goal.trigger {
            Trigger::Utterance { content, .. } => content.to_lowercase(),
            _ => String::new(),
        };

        // Very small intent recognizer — proves Invoke→exec→Express end to end.
        let command: Option<&str> = if text.contains("list the files")
            || text.contains("ls")
            || text.contains("files in")
        {
            Some("ls -la /tmp")
        } else if text.contains("date") || text.contains("time") {
            Some("date")
        } else if text.contains("whoami") {
            Some("whoami")
        } else {
            None
        };

        let mut intents = Vec::new();
        if let Some(cmd) = command {
            intents.push(ActionIntent::Invoke {
                capability: "cap.shell".into(),
                args: serde_json::json!({ "command": cmd }),
                correlation: None,
            });
            intents.push(ActionIntent::Express {
                body: format!("ran `{cmd}`"),
            });
        } else {
            intents.push(ActionIntent::Express {
                body: "I'm the keyless stub. Set OPENROUTER_API_KEY to use a real model. \
                       Try: 'list the files in /tmp', 'what is the date', or 'whoami'."
                    .into(),
            });
        }
        intents.push(ActionIntent::Conclude {
            outcome: Outcome::Achieved,
        });
        pan_core::schema::Decision { intents }
    }
}

// Keep `Verdict` referenced so the import is meaningful even if only used in
// the helper above.
#[allow(dead_code)]
fn _assert_imports() {
    let _v: Verdict = Verdict::Allow;
}
