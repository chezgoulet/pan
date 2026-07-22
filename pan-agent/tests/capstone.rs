//! The capstone: one `Agent.toml` becomes a running, governed agent that does a
//! real thing. Everything the loop needs — scope, governor, provider, and the
//! toolbox (capability registry + executor) — comes out of `assemble`, wired only
//! by the config file.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use pan_agent::{assemble_toml, builtin_registry};
use pan_core::events::{EventStream, MemorySink};
use pan_core::loop_engine::{Loop, Once};
use pan_core::pipeline::Pipeline;
use pan_core::schema::{Context, Goal, Trigger};

fn temp_root() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "pan_agent_capstone_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A full Agent.toml — a rules brain that, on a `note.requested` event, invokes
/// `cap.fs.write`; the `cap.fs` component is enabled and rooted; the `fs` family
/// is granted. Assemble it, drive one loop span with the triggering goal, and a
/// real file appears — all from config.
#[tokio::test]
async fn one_agent_toml_becomes_a_running_agent_that_writes_a_file() {
    let root = temp_root();
    let manifest = format!(
        r#"
[meta]
name = "note-taker"
persona = "assistant"

[persona]
instruction = "You take notes."
provider = "provider.rules"

[[persona.rules]]
when_event_topic = "note.requested"
then_invoke = {{ capability = "cap.fs.write", args = {{ path = "note.txt", content = "hello from config" }} }}

[caps]
enable = ["cap.state", "cap.fs"]

[caps.grant]
fs = true
state = true

[caps.settings."cap.fs"]
root = "{root}"
"#,
        root = root.display()
    );

    // Assemble everything from the manifest.
    let agent = assemble_toml(&manifest, &builtin_registry()).expect("assembles");
    assert_eq!(agent.scope.origin, "persona.assistant");

    // Wire the assembled pieces into a pipeline + loop. The toolbox is BOTH the
    // capability registry and the executor.
    let registry = agent.toolbox.registry();
    assert!(
        registry.lookup("cap.fs.write").is_some(),
        "the enabled cap.fs component must be in the registry"
    );
    let mut stream = EventStream::spawn(MemorySink::new());
    let pipeline = Pipeline {
        registry: &registry,
        governor: &agent.governor,
        executor: &agent.toolbox,
        events: &stream,
    };
    let lp = Loop {
        provider: agent.provider.as_ref(),
        pipeline: &pipeline,
        events: &stream,
        scope: agent.scope.clone(),
        token_tx: None,
        veto_source: pan_core::loop_engine::NO_VETO,
        stall_detector: None,
    };

    // Drive one span with the goal the rule reacts to.
    let mut obs = Once(Some(Goal {
        id: "g".into(),
        revision: 0,
        objective: "take a note".into(),
        trigger: Trigger::Event {
            topic: "note.requested".into(),
            payload: serde_json::json!({}),
        },
    }));
    let report = lp.run_span(&mut obs, &Context::default()).await;

    // The rule fired, the write was governed (fs granted) and executed, and a
    // real file exists — produced entirely from the Agent.toml.
    assert_eq!(report.effected, vec!["cap.fs.write"]);
    assert_eq!(
        std::fs::read_to_string(root.join("note.txt")).unwrap(),
        "hello from config"
    );
    stream.shutdown();
}

/// Enabling a capability the binary doesn't know is a clear load-time error.
#[test]
fn enabling_an_unknown_capability_is_a_load_error() {
    let manifest = r#"
[meta]
name = "x"
[persona]
provider = "provider.behaviortree"
[caps]
enable = ["cap.telepathy"]
"#;
    let result = assemble_toml(manifest, &builtin_registry());
    assert!(
        result.is_err(),
        "unknown capability component must fail assembly"
    );
}

/// `cap.fs` enabled without a root is a load-time error, not a silently unrooted
/// filesystem.
#[test]
fn cap_fs_without_a_root_is_a_load_error() {
    let manifest = r#"
[meta]
name = "x"
[persona]
provider = "provider.behaviortree"
[caps]
enable = ["cap.fs"]
"#;
    let result = assemble_toml(manifest, &builtin_registry());
    assert!(result.is_err(), "cap.fs without a root must fail assembly");
}
