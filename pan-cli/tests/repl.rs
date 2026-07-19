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
