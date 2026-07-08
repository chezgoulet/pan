//! # `pan` — the Wave 1 walking skeleton.
//!
//! Reads lines from stdin, treats each as a user utterance, asks the provider
//! for a decision, and enacts it: `Express` → printed to stdout, `Invoke` →
//! routed through the dispatch pipeline to `exec.local` (which runs
//! `cap.shell`). Every step is logged to stderr via `obs.logging`, and the
//! result of every effect is printed to stdout so the user sees what a
//! capability actually did.
//!
//! Provider: uses the generic OpenAI-compatible `provider.llm` when
//! `OPENROUTER_API_KEY` (or any backend via `PAN_BASE_URL`/`PAN_MODEL`) is set;
//! otherwise falls back to the core's deterministic stub provider so the
//! skeleton runs with zero config (proving the whole pipeline before any model
//! is involved).
//!
//! Exit test (manifest Wave 1): type `list the files in /tmp` → model emits
//! `Invoke(cap.shell, {command: "ls -la /tmp"})` → runs → reply printed →
//! action visible in logs.

use pan_core::events::EventStream;
use pan_core::pipeline::{Executor, Pipeline, Verdict};
use pan_core::plugins::exec_local::LocalExecutor;
use pan_core::plugins::gov_policy::Policy;
use pan_core::plugins::obs_logging::LogSink as ObsLog;
use pan_core::plugins::state_file::StateFile;
use pan_core::providers_llm::Llm;
use pan_core::registry::{CapabilityRegistry, Lifecycle};
use pan_core::schema::{
    ActionIntent, Capability, Context, Goal, Outcome, Provider, Trigger, Value,
};
use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

fn main() {
    // --- Capabilities the agent may invoke ---------------------------------
    let mut registry = CapabilityRegistry::new();
    registry
        .register(Capability::new("cap.shell", "Run a shell command. Args: { command: string, cwd?: string }", serde_json::json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": {"type": "string"},
                    "cwd": {"type": "string"}
                }
            })))
        .expect("register cap.shell");
    registry
        .register(Capability::new("cap.state_write", "Write a key/value into process memory. Args: { path, value }", serde_json::json!({
                "type": "object",
                "required": ["path", "value"],
                "properties": {
                    "path": {"type": "string"},
                    "value": {}
                }
            })))
        .expect("register cap.state_write");

    // cap.fs — governed file read/write.
    registry
        .register(Capability::new("cap.fs", "Read or write a file. Args: { path, op: 'read'|'write', content?: string }", serde_json::json!({
                "type": "object",
                "required": ["path", "op"],
                "properties": {
                    "path": {"type": "string"},
                    "op": {"type": "string", "enum": ["read", "write"]},
                    "content": {"type": "string"}
                }
            })))
        .expect("register cap.fs");
    // cap.http — outbound web requests.
    registry
        .register(Capability::new("cap.http", "Make an outbound HTTP request. Args: { url, method?: string, body?: string, headers?: object }", serde_json::json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": {"type": "string"},
                    "method": {"type": "string"},
                    "body": {"type": "string"},
                    "headers": {"type": "object"}
                }
            })))
        .expect("register cap.http");

    // cap.pair.invite — generate a pairing code for inbound channel admission.
    registry
        .register(Capability::new("cap.pair.invite", "Generate a single-use pairing code", serde_json::json!({
                "type": "object",
                "required": ["user"],
                "properties": {
                    "user": {"type": "string"}
                }
            })))
        .expect("register cap.pair.invite");

    // --- Governor: governance policy with pairing/allowlist for inbound channels
    let governor = Arc::new(Policy::new());

    // Pre-authorize the default CLI user so pairing is opt-in for channels.
    // Set PAN_PAIRING_CODE to enable code-based pairing for demo/testing.
    // In production, channels drive their own admission via governor.is_paired().
    governor.authorize("user");
    if let Ok(code) = std::env::var("PAN_PAIRING_CODE") {
        eprintln!("[pan] pairing code set via PAN_PAIRING_CODE: {code}");
        governor.inject_code(&code, "preauthorized");
    }

    // Store a clone for the cap.pair.invite handler closure.
    let gov_for_pair = Arc::clone(&governor);

    // Disk-persistent state (Wave 2). Path overridable via PAN_STATE_FILE so the
    // restart test is reproducible; defaults to a temp file.
    let state_path = std::env::var("PAN_STATE_FILE")
        .unwrap_or_else(|_| "/tmp/pan-state.json".to_string());
    let state = Arc::new(StateFile::new(&state_path));
    // Load existing state into THIS shared instance (the one the handler uses)
    // up front, so a restart picks up what a prior run wrote.
    if let Err(e) = state.load() {
        eprintln!("[pan] warning: could not load state file {state_path}: {e}");
    }
    eprintln!("[pan] state file: {state_path} ({} keys loaded)", state.keys().len());

    // exec.local, wrapped so every effect result is recorded synchronously
    // (on the pipeline's own thread) for the REPL to display. The off-thread
    // event stream is for logging/audit only; we must not depend on its timing
    // to surface results.
    let effects: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
    let inner = LocalExecutor::new();
    // cap.state_write persists into the on-disk state store for real.
    let state_for_handler = Arc::clone(&state);
    inner.register("cap.state_write", move |args| {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| pan_core::pipeline::ExecError("cap.state_write needs `path`".into()))?;
        let value = args.get("value").cloned().unwrap_or(Value::Null);
        state_for_handler.write(path, value.clone());
        Ok(serde_json::json!({ "wrote": path, "value": value }))
    });
    // cap.fs and cap.http are pure functions; wire the handlers.
    inner.register("cap.fs", |args| pan_core::plugins::cap_fs::handle_fs(args));
    inner.register("cap.http", |args| pan_core::plugins::cap_http::handle_http(args));
    // cap.pair.invite: generate a pairing code for a new user.
    inner.register("cap.pair.invite", {
        let gov = Arc::clone(&gov_for_pair);
        move |args| {
            let user = args.get("user").and_then(|v| v.as_str()).unwrap_or("unknown");
            let code = gov.generate_code(user);
            Ok(serde_json::json!({
                "code": code,
                "message": format!("Pairing code for {user}: {code}")
            }))
        }
    });

    // Optional MCP bridge: if PAN_MCP_CMD is set (e.g. "python3 /path/server.py"),
    // spawn the server, enumerate its tools, and register each as a capability.
    // The session is held in an Arc so handler closures can call into it.
    let mcp_session: Option<Arc<pan_core::plugins::cap_mcp::McpSession>> =
        if let Ok(cmd) = std::env::var("PAN_MCP_CMD") {
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            if parts.is_empty() {
                None
            } else {
                match pan_core::plugins::cap_mcp::McpSession::spawn(parts[0], &parts[1..]) {
                    Ok(s) => match s.tools() {
                        Ok(tools) => {
                            let arc = Arc::new(s);
                            for t in &tools {
                                let cap_id = format!("cap.mcp.{}", t.name);
                                // Register the capability so the model can name it.
                                let _ = registry.register(Capability::new(cap_id.clone(), t.description.clone(), t.input_schema.clone()));
                                // Register the handler: route into the MCP session.
                                let sess = Arc::clone(&arc);
                                let tool_name = t.name.clone();
                                inner.register(&cap_id, move |args| {
                                    sess.call(&tool_name, args)
                                        .map_err(|e| {
                                            pan_core::pipeline::ExecError(e.0.clone())
                                        })
                                });
                            }
                            eprintln!(
                                "[pan] MCP: bridged {} tool(s) from {}",
                                tools.len(),
                                parts[0]
                            );
                            Some(arc)
                        }
                        Err(e) => {
                            eprintln!("[pan] MCP tools/list failed: {e}");
                            None
                        }
                    },
                    Err(e) => {
                        eprintln!("[pan] MCP spawn failed: {e}");
                        None
                    }
                }
            }
        } else {
            None
        };
    let _ = &mcp_session; // kept alive for the process lifetime
    let executor = RecordingExecutor {
        inner,
        effects: Arc::clone(&effects),
    };

    // Lifecycle: register + provision + validate (catches id conflicts early).
    // state.file loads its file during provision; report if it can't.
    let mut lifecycle = Lifecycle::new();
    lifecycle.register(Box::new(Policy::new()));
    lifecycle.register(Box::new(StateFile::new(&state_path)));
    if let Err(e) = lifecycle.provision() {
        eprintln!("lifecycle provision failed: {e}");
        std::process::exit(1);
    }

    // --- Provider selection -------------------------------------------------
    // Real model when a key/backend is configured; backend is fully overridable
    // via PAN_BASE_URL / PAN_MODEL (OpenRouter, local llama.cpp, mock, ...).
    // Falls back to the deterministic keyless stub otherwise.
    let has_model = std::env::var("OPENROUTER_API_KEY").is_ok()
        || std::env::var("PAN_BASE_URL").is_ok();
    let provider: Box<dyn Provider> = if has_model {
        let base = std::env::var("PAN_BASE_URL")
            .unwrap_or_else(|_| pan_core::providers_llm::DEFAULT_BASE_URL.to_string());
        let model = std::env::var("PAN_MODEL")
            .unwrap_or_else(|_| pan_core::providers_llm::DEFAULT_MODEL.to_string());
        let key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
        Box::new(Llm::new(&base, &model, &key))
    } else {
        eprintln!(
            "[pan] no model configured — using deterministic stub provider.\n\
             [pan] set OPENROUTER_API_KEY (or PAN_BASE_URL) to talk to a real model."
        );
        Box::new(StubShellProvider)
    };

    // --- Event stream + sink (logging/audit only) --------------------------
    let (stream, guard) = EventStream::spawn(ObsLog::new());

    let pipeline = Pipeline {
        registry: &registry,
        governor: governor.as_ref(),
        executor: &executor,
        events: &stream,
    };

    let loop_ = pan_core::loop_engine::Loop {
        provider: provider.as_ref(),
        admitter: &pan_core::loop_engine::AdmitAll,
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

        // ---- Admission check: pairing / allowlist -----------------------
        let user_id = "user"; // CLI user; channel plugins would use real ids
        if !governor.is_paired(user_id) {
            // Unpaired user: check if this is a pairing attempt.
            if let Some(code) = line.strip_prefix("/pair ") {
                if governor.pair_with_code(user_id, code) {
                    println!("✅ Paired successfully! You can now chat with the agent.");
                } else {
                    println!("❌ Invalid pairing code. Use `/pair <code>` with a valid code.");
                }
            } else {
                println!(
                    "❌ Not authorized. This channel requires pairing.\n\
                     An admin must generate a pairing code; then send `/pair <code>`."
                );
            }
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
            &mut pan_core::loop_engine::Once(Some(pan_core::schema::SpanContext {
                persona: pan_core::schema::PersonaId("user".into()),
                goal,
            })),
            &Context::default(),
        );

        // Surface expressions to the user.
        for body in &report.expressed {
            println!("{body}");
        }
        // Surface effect results (recorded synchronously by RecordingExecutor).
        for (cap, result) in effects.lock().unwrap().drain(..) {
            if cap == "cap.shell" {
                if let Some(out) = result.get("stdout").and_then(|v| v.as_str()) {
                    let trimmed = out.trim_end();
                    if !trimmed.is_empty() {
                        println!("{trimmed}");
                    }
                }
                if let Some(err) = result.get("stderr").and_then(|v| v.as_str()) {
                    let trimmed = err.trim_end();
                    if !trimmed.is_empty() {
                        eprintln!("[stderr] {trimmed}");
                    }
                }
            } else {
                println!("[{cap}] {result}");
            }
        }
        print!("pan> ");
        let _ = stdout.flush();
    }

    stream.shutdown(guard);
    println!("\n[pan] bye.");
}

/// Wraps `LocalExecutor`, forwarding execution and recording each result into a
/// shared vec **on the calling thread** (the pipeline runs effects
/// synchronously), so the REPL can display what a capability returned without
/// racing the off-thread event consumer.
struct RecordingExecutor {
    inner: LocalExecutor,
    effects: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Executor for RecordingExecutor {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn execute(&self, capability: &str, args: &Value) -> Result<Value, pan_core::pipeline::ExecError> {
        let result = self.inner.execute(capability, args);
        if let Ok(v) = &result {
            self.effects
                .lock()
                .unwrap()
                .push((capability.to_string(), v.clone()));
        }
        result
    }
}

/// Deterministic stub: turns a natural-language-ish request into a
/// `cap.shell` Invoke by naive keyword match, so the skeleton demonstrates the
/// full pipeline without a model. DEV ONLY; superseded by `provider.llm` the
/// moment a model is configured.
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
                body: "I'm the keyless stub. Set OPENROUTER_API_KEY or PAN_BASE_URL to use a \
                       real model. Try: 'list the files in /tmp', 'what is the date', or \
                       'whoami'."
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
