//! The REPL, exercised end-to-end over in-memory byte streams — no terminal.
//! Assemble an `Agent.toml`, feed it lines, and check the replies.

use pan_agent::{assemble_toml, builtin_registry};
use pan_cli::run_session_on_bytes;

fn agent(manifest: &str) -> pan_agent::AssembledAgent {
    assemble_toml(manifest, &builtin_registry()).expect("assembles")
}

/// The out-of-the-box conversational path: `provider.echo` answers each line.
#[tokio::test]
async fn echo_agent_replies_to_each_line() {
    let a = agent(
        r#"
[meta]
name = "chatty"
[persona]
provider = "provider.echo"
"#,
    );
    let out = run_session_on_bytes(&a, b"hello\nhow are you\n/quit\n")
        .await
        .unwrap();

    assert!(out.contains("you said: hello"), "got: {out:?}");
    assert!(out.contains("you said: how are you"), "got: {out:?}");
}

/// `/quit` ends the session; nothing after it is processed.
#[tokio::test]
async fn quit_stops_the_session() {
    let a = agent(
        r#"
[meta]
name = "chatty"
[persona]
provider = "provider.echo"
"#,
    );
    let out = run_session_on_bytes(&a, b"first\n/quit\nsecond\n")
        .await
        .unwrap();
    assert!(out.contains("you said: first"));
    assert!(
        !out.contains("second"),
        "input after /quit must be ignored: {out:?}"
    );
}

/// A persona `prefix` from the manifest shapes the reply — config reaching the
/// provider through the CLI.
#[tokio::test]
async fn persona_prefix_from_config_shapes_the_reply() {
    let a = agent(
        r#"
[meta]
name = "parrot"
[persona]
provider = "provider.echo"
prefix = "squawk"
"#,
    );
    let out = run_session_on_bytes(&a, b"crackers\n").await.unwrap();
    assert!(out.contains("squawk: crackers"), "got: {out:?}");
}

/// Blank lines are skipped, not turned into empty turns.
#[tokio::test]
async fn blank_lines_are_ignored() {
    let a = agent(
        r#"
[meta]
name = "chatty"
[persona]
provider = "provider.echo"
"#,
    );
    let out = run_session_on_bytes(&a, b"\n\nhi\n\n").await.unwrap();
    assert_eq!(
        out.trim(),
        "you said: hi",
        "only the non-blank line replies: {out:?}"
    );
}

/// The full interactive stack: `provider.command` drives real capabilities.
/// `run echo` executes and its stdout is shown; `remember`/`recall` round-trip
/// through `cap.state`. Everything is governed by the manifest's grants.
#[tokio::test]
async fn command_agent_runs_shell_and_round_trips_state() {
    let a = agent(
        r#"
[meta]
name = "doer"
[persona]
provider = "provider.command"
[caps]
enable = ["cap.shell", "cap.state"]
[caps.grant]
shell = true
state = true
"#,
    );
    let out = run_session_on_bytes(
        &a,
        b"run echo hello world\nremember pet cat\nrecall pet\n/quit\n",
    )
    .await
    .unwrap();

    assert!(
        out.contains("$ echo hello world"),
        "narration missing: {out:?}"
    );
    assert!(out.contains("hello world"), "shell stdout missing: {out:?}");
    assert!(
        out.contains("remembered `pet`"),
        "set narration missing: {out:?}"
    );
    assert!(out.contains("= \"cat\""), "recalled value missing: {out:?}");
}

/// Governance across the CLI: `cap.shell` is enabled (it exists) but the persona
/// is NOT granted `shell`, so the invoke is denied at `govern` and the CLI reports
/// the failure — the command cannot escape its scope.
#[tokio::test]
async fn an_ungranted_capability_is_denied_and_reported() {
    let a = agent(
        r#"
[meta]
name = "restricted"
[persona]
provider = "provider.command"
[caps]
enable = ["cap.shell"]
[caps.grant]
state = true
"#,
    );
    let out = run_session_on_bytes(&a, b"run echo should-not-run\n/quit\n")
        .await
        .unwrap();

    assert!(
        out.contains("[error] capability `cap.shell.run` failed"),
        "ungranted shell must be denied and reported: {out:?}"
    );
}
