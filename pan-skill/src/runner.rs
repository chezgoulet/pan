//! # The skill runner — a governed subprocess bridge (ADR 0001, D2).
//!
//! [`SkillRunner`] spawns a skill as a subprocess and services every capability
//! the skill invokes by routing it through a [`ScopedInvoker`] — i.e. through the
//! full `resolve → validate → govern → execute` pipeline, under the scope the
//! skill was granted. The subprocess is handed **no capability object**; its only
//! sanctioned channel to the world is the invoke protocol, so a skill cannot
//! reach anything the governor would not permit. The `Governed` type-state
//! invariant is untouched: the subprocess has no Rust handle at all, and the
//! runner reaches the executor only via `ScopedInvoker::invoke`.
//!
//! Because the bridge is async, a skill blocked awaiting an invoke result is a
//! *suspended future*, not a blocked thread. And because the child is spawned
//! `kill_on_drop`, dropping the run future (e.g. the loop abandoned a superseded
//! decision mid-skill) tears the subprocess down with it.
//!
//! ## Isolation: what this guarantees, and what it does not (yet)
//!
//! **Guaranteed:** the skill has no Pan capability object, so every *sanctioned*
//! effect flows through the governed pipeline. **Not yet enforced:** OS-level
//! denial of *ambient* fs/network (a skill that calls `open()` directly still
//! hits the real OS). That hardening — Linux namespaces / seccomp, or a wrapper
//! like `bwrap`/`nsjail` — plugs in at [`SkillRunner::with_program`] (point it at
//! the sandbox launcher). The runner does not fake it; the honest boundary today
//! is "all Pan-sanctioned I/O is governed."

use std::path::{Path, PathBuf};
use std::process::Stdio;

use pan_core::invoker::{InvokeError, ScopedInvoker};
use pan_core::schema::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::protocol::{FromSkill, RESULT_TYPE};

/// The embedded Python client library the runner materializes so skills can
/// `import pan`. Kept in sync with the protocol here.
pub const PAN_PY: &str = include_str!("pan.py");

/// Why a skill run did not produce a return value.
#[derive(Debug)]
pub enum SkillError {
    /// The subprocess could not be spawned (e.g. `python3` not found).
    Spawn(String),
    /// A line from the skill was not valid protocol JSON.
    Protocol { line: String, detail: String },
    /// The skill closed its stdout without sending a `return` — it crashed or
    /// exited early. Carries the captured stderr (usually a Python traceback)
    /// and the exit code, for diagnosis.
    NoReturn { stderr: String, code: Option<i32> },
    /// An I/O error talking to the subprocess.
    Io(String),
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillError::Spawn(e) => write!(f, "could not spawn skill process: {e}"),
            SkillError::Protocol { line, detail } => {
                write!(f, "malformed skill message ({detail}): {line}")
            }
            SkillError::NoReturn { stderr, code } => write!(
                f,
                "skill exited (code {:?}) without returning; stderr:\n{}",
                code, stderr
            ),
            SkillError::Io(e) => write!(f, "skill I/O error: {e}"),
        }
    }
}

impl std::error::Error for SkillError {}

/// Runs skill subprocesses, materializing the `pan.py` client library once so
/// spawned skills can `import pan`.
pub struct SkillRunner {
    program: String,
    program_args: Vec<String>,
    lib_dir: PathBuf,
}

impl SkillRunner {
    /// Create a runner that launches skills with `python3`, writing the embedded
    /// `pan.py` into `lib_dir` (which becomes the skill's `PYTHONPATH`).
    pub fn new(lib_dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        Self::with_program("python3", std::iter::empty::<String>(), lib_dir)
    }

    /// Like [`new`](Self::new) but with an explicit launcher and leading args —
    /// the seam for OS-level sandboxing. For example, to run each skill inside a
    /// bubblewrap sandbox with no network, point `program` at `bwrap` and pass
    /// its args, followed by `python3`; the skill path is appended last.
    pub fn with_program(
        program: impl Into<String>,
        program_args: impl IntoIterator<Item = impl Into<String>>,
        lib_dir: impl Into<PathBuf>,
    ) -> std::io::Result<Self> {
        let lib_dir = lib_dir.into();
        std::fs::create_dir_all(&lib_dir)?;
        std::fs::write(lib_dir.join("pan.py"), PAN_PY)?;
        Ok(Self {
            program: program.into(),
            program_args: program_args.into_iter().map(Into::into).collect(),
            lib_dir,
        })
    }

    /// Run `skill_path` to completion, servicing each capability it invokes
    /// through `invoker` (each under the invoker's bound scope), and returning
    /// the skill's `return` value.
    ///
    /// `input` is handed to the skill as JSON via `PAN_SKILL_INPUT` (readable in
    /// Python as `pan.input()`), keeping the stdin/stdout channel purely the
    /// invoke ↔ result conversation.
    pub async fn run(
        &self,
        skill_path: &Path,
        input: &Value,
        invoker: &dyn ScopedInvoker,
    ) -> Result<Value, SkillError> {
        let mut child = Command::new(&self.program)
            .args(&self.program_args)
            .arg(skill_path)
            .env("PYTHONPATH", &self.lib_dir)
            .env("PYTHONUNBUFFERED", "1")
            .env("PAN_SKILL_INPUT", input.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SkillError::Spawn(e.to_string()))?;

        let stdout = child.stdout.take().expect("stdout piped");
        let mut stdin = child.stdin.take().expect("stdin piped");
        let stderr = child.stderr.take().expect("stderr piped");

        // Drain stderr concurrently into a buffer so a chatty skill can't deadlock
        // on a full stderr pipe, and so tracebacks are available on failure.
        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            let mut reader = BufReader::new(stderr);
            let _ = reader.read_to_string(&mut buf).await;
            buf
        });

        let mut lines = BufReader::new(stdout).lines();
        let outcome = loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let msg: FromSkill = match serde_json::from_str(&line) {
                        Ok(m) => m,
                        Err(e) => {
                            break Err(SkillError::Protocol {
                                line,
                                detail: e.to_string(),
                            })
                        }
                    };
                    match msg {
                        FromSkill::Return { value } => break Ok(value),
                        FromSkill::Invoke {
                            id,
                            capability,
                            args,
                        } => {
                            // The load-bearing line: the subprocess's request is
                            // run through the GOVERNED pipeline, under the skill's
                            // bound scope. A denial here is a denial the skill sees.
                            let response = match invoker.invoke(&capability, &args).await {
                                Ok(value) => result_line(id, true, Some(value), None),
                                Err(err) => {
                                    result_line(id, false, None, Some(invoke_error_info(&err)))
                                }
                            };
                            if let Err(e) = stdin.write_all(response.as_bytes()).await {
                                break Err(SkillError::Io(e.to_string()));
                            }
                            if let Err(e) = stdin.flush().await {
                                break Err(SkillError::Io(e.to_string()));
                            }
                        }
                    }
                }
                // EOF: the skill closed stdout without ever returning.
                Ok(None) => {
                    break Err(SkillError::NoReturn {
                        stderr: String::new(),
                        code: None,
                    })
                }
                Err(e) => break Err(SkillError::Io(e.to_string())),
            }
        };

        // Reap the child (closing stdin by dropping it) and collect stderr.
        drop(stdin);
        let status = child.wait().await.ok();
        let stderr = stderr_task.await.unwrap_or_default();

        // Enrich a bare NoReturn with the diagnostics we now have.
        match outcome {
            Err(SkillError::NoReturn { .. }) => Err(SkillError::NoReturn {
                stderr,
                code: status.and_then(|s| s.code()),
            }),
            other => other,
        }
    }
}

/// Serialize one `result` line (with trailing newline) for the skill's stdin.
fn result_line(id: u64, ok: bool, value: Option<Value>, error: Option<Value>) -> String {
    let mut msg = serde_json::json!({ "type": RESULT_TYPE, "id": id, "ok": ok });
    if let Some(v) = value {
        msg["value"] = v;
    }
    if let Some(e) = error {
        msg["error"] = e;
    }
    let mut line = msg.to_string();
    line.push('\n');
    line
}

/// Project an [`InvokeError`] into the `{kind, message}` the Python side reads.
/// `kind == "denied"` is what `pan.invoke` raises as `PanDenied`.
fn invoke_error_info(err: &InvokeError) -> Value {
    let kind = match err {
        InvokeError::NotFound { .. } => "not_found",
        InvokeError::InvalidArgs { .. } => "invalid_args",
        InvokeError::Denied { .. } => "denied",
        InvokeError::Failed { .. } => "failed",
    };
    serde_json::json!({ "kind": kind, "message": err.to_string() })
}
