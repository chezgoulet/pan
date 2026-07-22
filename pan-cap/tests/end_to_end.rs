//! The executor/capability model, composed end-to-end: a `Toolbox` of real
//! capability components becomes the pipeline's registry *and* executor, and a
//! scoped governor gates what actually runs. This is the "assembled agent does
//! things" proof — real files written, real governance enforced.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use pan_cap::{FsCaps, StateCaps};
use pan_core::events::{EventStream, MemorySink};
use pan_core::loop_engine::{Loop, Once};
use pan_core::pipeline::{EffectRequest, Pipeline, PipelineError, ScopedGovernor};
use pan_core::providers::behaviortree::{BehaviorTreeProvider, Node};
use pan_core::schema::{Context, Goal, Scope, Trigger};
use pan_core::toolbox::Toolbox;

fn temp_root() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "pan_cap_e2e_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn toolbox(root: &std::path::Path) -> Toolbox {
    Toolbox::new()
        .with(Box::new(StateCaps::new()))
        .unwrap()
        .with(Box::new(FsCaps::new(root)))
        .unwrap()
}

/// A persona granted `cap.fs` writes a file through the full pipeline; a
/// different, ungranted origin is denied at `govern` and the file is untouched.
#[tokio::test]
async fn scoped_fs_write_runs_when_granted_and_is_denied_otherwise() {
    let root = temp_root();
    let tb = toolbox(&root);
    let reg = tb.registry();
    let gov = ScopedGovernor::new().grant("persona.assistant", ["cap.fs", "cap.state"]);
    let mut stream = EventStream::spawn(MemorySink::new());
    let pipe = Pipeline {
        registry: &reg,
        governor: &gov,
        executor: &tb,
        events: &stream,
    };

    // Granted origin: the write actually happens.
    let ok = pipe
        .dispatch(EffectRequest {
            capability: "cap.fs.write".into(),
            args: serde_json::json!({ "path": "note.txt", "content": "hello" }),
            correlation: None,
            scope: Scope::new("persona.assistant"),
        })
        .await;
    assert!(ok.is_ok(), "granted persona should write, got {ok:?}");
    assert_eq!(
        std::fs::read_to_string(root.join("note.txt")).unwrap(),
        "hello"
    );

    // Ungranted origin: denied at govern, and the file is NOT overwritten.
    let denied = pipe
        .dispatch(EffectRequest {
            capability: "cap.fs.write".into(),
            args: serde_json::json!({ "path": "note.txt", "content": "tampered" }),
            correlation: None,
            scope: Scope::new("skill.rogue"),
        })
        .await;
    assert!(
        matches!(denied, Err(PipelineError::Rejected(_))),
        "ungranted origin must be denied, got {denied:?}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("note.txt")).unwrap(),
        "hello",
        "a denied write must not reach the filesystem"
    );
    stream.shutdown();
}

/// Even a persona granted `cap.fs` cannot escape its root: the jail refuses `..`
/// at the executor, surfacing as a pipeline Execution error (defense in depth,
/// independent of the governor).
#[tokio::test]
async fn granted_persona_still_cannot_escape_the_fs_root() {
    let root = temp_root();
    let tb = toolbox(&root);
    let reg = tb.registry();
    let gov = ScopedGovernor::new().grant("persona.assistant", ["cap.fs"]);
    let mut stream = EventStream::spawn(MemorySink::new());
    let pipe = Pipeline {
        registry: &reg,
        governor: &gov,
        executor: &tb,
        events: &stream,
    };

    let escape = pipe
        .dispatch(EffectRequest {
            capability: "cap.fs.read".into(),
            args: serde_json::json!({ "path": "../../etc/passwd" }),
            correlation: None,
            scope: Scope::new("persona.assistant"),
        })
        .await;
    assert!(
        matches!(escape, Err(PipelineError::Execution { .. })),
        "traversal must fail at the executor jail, got {escape:?}"
    );
    stream.shutdown();
}

/// The walking skeleton: a provider (here a behavior tree — no LLM needed)
/// decides to invoke `cap.fs.write`, and driving one loop span writes the file.
/// Provider → loop → govern → real capability, all composed.
#[tokio::test]
async fn a_provider_driving_the_loop_writes_a_real_file() {
    let root = temp_root();
    let tb = toolbox(&root);
    let reg = tb.registry();
    let gov = ScopedGovernor::new().grant("persona.assistant", ["cap.fs"]);
    let mut stream = EventStream::spawn(MemorySink::new());
    let pipe = Pipeline {
        registry: &reg,
        governor: &gov,
        executor: &tb,
        events: &stream,
    };

    // A behavior tree that writes a file, then succeeds.
    let bt = BehaviorTreeProvider {
        root: vec![
            Node::Action {
                capability: "cap.fs.write".into(),
                args: serde_json::json!({ "path": "from_loop.txt", "content": "written by the loop" }),
            },
            Node::Succeed,
        ],
    };
    let lp = Loop {
        provider: &bt,
        pipeline: &pipe,
        events: &stream,
        scope: Scope::new("persona.assistant"),
        token_tx: None,
        veto_source: pan_core::loop_engine::NO_VETO,
        stall_detector: None,
    };
    let mut obs = Once(Some(Goal {
        id: "g".into(),
        revision: 0,
        objective: "write a note".into(),
        trigger: Trigger::Tick { sequence: 0 },
    }));
    let report = lp.run_span(&mut obs, &Context::default()).await;

    assert_eq!(report.effected, vec!["cap.fs.write"]);
    assert_eq!(
        std::fs::read_to_string(root.join("from_loop.txt")).unwrap(),
        "written by the loop"
    );
    stream.shutdown();
}
