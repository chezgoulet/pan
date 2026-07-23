//! End-to-end tests: a real `python3` skill reaching the world only through the
//! governed pipeline. These spawn a subprocess, so they need `python3` on PATH;
//! if it is absent the test logs a skip and passes (matching how the daemon
//! gates its endpoint-dependent tests).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use pan_core::events::{EventStream, MemorySink};
use pan_core::invoker::PipelineInvoker;
use pan_core::pipeline::{EchoExecutor, Pipeline, ScopedGovernor};
use pan_core::registry::CapabilityRegistry;
use pan_core::schema::{Capability, Scope, Value};
use pan_skill::{SkillError, SkillRunner};

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A throwaway directory that also becomes the skill's `PYTHONPATH` (where the
/// runner drops `pan.py`) and the home of the skill file.
fn scratch(skill_src: &str) -> (PathBuf, PathBuf) {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "pan_skill_test_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let skill = dir.join("skill.py");
    std::fs::write(&skill, skill_src).unwrap();
    (dir, skill)
}

fn registry(caps: &[&str]) -> CapabilityRegistry {
    let mut r = CapabilityRegistry::new();
    for id in caps {
        r.register(Capability {
            id: (*id).into(),
            summary: String::new(),
            args_schema: serde_json::json!({ "type": "object" }),
        })
        .unwrap();
    }
    r
}

#[tokio::test]
async fn skill_invoke_is_governed_and_returns() {
    if !python3_available() {
        eprintln!("skipping: python3 not available");
        return;
    }
    // The skill reads a file through cap.fs.read (within its grant) and returns
    // something derived from the (echoed) result.
    let (dir, skill) = scratch(
        r#"
import pan
data = pan.invoke("cap.fs.read", {"path": "notes.md"})
pan.done({"echoed_cap": data["executed"], "got_path": data["args"]["path"]})
"#,
    );

    let reg = registry(&["cap.fs.read", "cap.shell.run"]);
    let gov = ScopedGovernor::new().grant("skill.notetaker", ["cap.fs"]);
    let mut stream = EventStream::spawn(MemorySink::new());
    let pipe = Pipeline {
        registry: &reg,
        governor: &gov,
        executor: &EchoExecutor,
        events: &stream,
        hooks: vec![],
    };
    let invoker = PipelineInvoker::new(&pipe, Scope::new("skill.notetaker"));
    let runner = SkillRunner::new(&dir).unwrap();

    let out = runner
        .run(&skill, &Value::Null, &invoker)
        .await
        .expect("skill should return");

    // The echo executor returns {"executed": cap, "args": args}; the skill lifted
    // fields out of that, proving the round-trip through the governed pipeline.
    assert_eq!(out["echoed_cap"], "cap.fs.read");
    assert_eq!(out["got_path"], "notes.md");
    stream.shutdown();
}

#[tokio::test]
async fn out_of_scope_invoke_is_denied_to_the_skill() {
    if !python3_available() {
        eprintln!("skipping: python3 not available");
        return;
    }
    // The skill tries a capability outside its grant; the governor's denial must
    // surface INSIDE the subprocess as PanDenied — governance crosses the boundary.
    let (dir, skill) = scratch(
        r#"
import pan
try:
    pan.invoke("cap.shell.run", {"cmd": "rm -rf /"})
    pan.done({"denied": False, "note": "escaped its scope!"})
except pan.PanDenied as e:
    pan.done({"denied": True, "kind": e.kind})
"#,
    );

    let reg = registry(&["cap.fs.read", "cap.shell.run"]);
    let gov = ScopedGovernor::new().grant("skill.notetaker", ["cap.fs"]);
    let mut stream = EventStream::spawn(MemorySink::new());
    let pipe = Pipeline {
        registry: &reg,
        governor: &gov,
        executor: &EchoExecutor,
        events: &stream,
        hooks: vec![],
    };
    let invoker = PipelineInvoker::new(&pipe, Scope::new("skill.notetaker"));
    let runner = SkillRunner::new(&dir).unwrap();

    let out = runner.run(&skill, &Value::Null, &invoker).await.unwrap();
    assert_eq!(
        out["denied"], true,
        "the out-of-scope capability must be denied inside the skill, got {out}"
    );
    assert_eq!(out["kind"], "denied");
    stream.shutdown();
}

#[tokio::test]
async fn skill_input_round_trips_via_env() {
    if !python3_available() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let (dir, skill) = scratch(
        r#"
import pan
pan.done({"echo": pan.input()})
"#,
    );

    let reg = registry(&[]);
    let gov = ScopedGovernor::new().grant("skill.x", ["cap"]);
    let mut stream = EventStream::spawn(MemorySink::new());
    let pipe = Pipeline {
        registry: &reg,
        governor: &gov,
        executor: &EchoExecutor,
        events: &stream,
        hooks: vec![],
    };
    let invoker = PipelineInvoker::new(&pipe, Scope::new("skill.x"));
    let runner = SkillRunner::new(&dir).unwrap();

    let input = serde_json::json!({ "hello": "world", "n": 3 });
    let out = runner.run(&skill, &input, &invoker).await.unwrap();
    assert_eq!(out["echo"], input);
    stream.shutdown();
}

#[tokio::test]
async fn skill_that_crashes_surfaces_its_stderr() {
    if !python3_available() {
        eprintln!("skipping: python3 not available");
        return;
    }
    // A skill that raises before returning: the runner must not hang, and must
    // surface the traceback rather than a silent success.
    let (dir, skill) = scratch(
        r#"
import pan
raise ValueError("boom in the skill")
"#,
    );

    let reg = registry(&[]);
    let gov = ScopedGovernor::new().grant("skill.x", ["cap"]);
    let mut stream = EventStream::spawn(MemorySink::new());
    let pipe = Pipeline {
        registry: &reg,
        governor: &gov,
        executor: &EchoExecutor,
        events: &stream,
        hooks: vec![],
    };
    let invoker = PipelineInvoker::new(&pipe, Scope::new("skill.x"));
    let runner = SkillRunner::new(&dir).unwrap();

    let err = runner
        .run(&skill, &Value::Null, &invoker)
        .await
        .expect_err("a crashing skill must be an error, not a value");
    match err {
        SkillError::NoReturn { stderr, code } => {
            assert!(
                stderr.contains("boom in the skill"),
                "stderr should carry the traceback, got: {stderr}"
            );
            assert_ne!(code, Some(0), "a crashed skill should not exit 0");
        }
        other => panic!("expected NoReturn, got {other}"),
    }
    stream.shutdown();
}
