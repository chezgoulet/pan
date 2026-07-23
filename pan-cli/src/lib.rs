//! # pan-cli — run an `Agent.toml` as an interactive agent.
//!
//! The thin layer that turns an [`AssembledAgent`](pan_agent::AssembledAgent) into
//! a running REPL: each input line becomes a `Goal` (an `Utterance`), one loop
//! span decides + enacts it under the agent's scope + governor + toolbox, and the
//! provider's `Express` output is written back. This is the `channel.cli` of the
//! plan, and it is *thin* precisely because `assemble` already produced the whole
//! graph — the CLI just feeds it lines and prints replies.
//!
//! The intelligence is the configured provider: `provider.echo` answers out of
//! the box; a rules/behavior-tree brain reacts to events/signals; a real LLM
//! provider (behind the same trait) makes it conversational. The harness here is
//! provider-agnostic — the whole point of Pan's vocabulary.

use pan_agent::AssembledAgent;
use pan_core::events::{DiscardSink, EventStream};
use pan_core::loop_engine::{Loop, Once};
use pan_core::pipeline::Pipeline;
use pan_core::schema::{Context, Goal, Trigger, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Drive `agent` as a REPL: read lines from `reader`, write replies to `writer`,
/// until EOF or a `/quit` line. Each line is one discrete turn.
///
/// Everything the loop needs comes from the [`AssembledAgent`]: `toolbox.registry()`
/// is the capability registry, `&toolbox` the executor, `governor` the govern
/// stage, and `provider` + `scope` drive decisions. Effects are governed exactly
/// as everywhere else — the CLI is just another origin of goals.
pub async fn run_session<R, W>(
    agent: &AssembledAgent,
    reader: R,
    writer: &mut W,
) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let registry = agent.toolbox.registry();
    let mut stream = EventStream::spawn(DiscardSink);
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
        compactor: None,
        context_budget: None,
        evaluator: None,
    };

    let mut lines = reader.lines();
    let mut turn: u64 = 0;
    while let Some(raw) = lines.next_line().await? {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line == "/quit" || line == "/exit" {
            break;
        }

        turn += 1;
        let goal = Goal {
            id: format!("turn-{turn}"),
            revision: 0,
            objective: line.to_string(),
            trigger: Trigger::Utterance {
                from: "user".into(),
                content: line.to_string(),
            },
        };
        let ctx = match &agent.context_assembler {
            Some(assembler) => assembler.assemble(&goal).await,
            None => Context::default(),
        };
        let goal_for_commit = goal.clone();
        let mut obs = Once(Some(goal));
        let report = lp.run_span(&mut obs, &ctx).await;

        if let Some(assembler) = &agent.context_assembler {
            assembler.commit(&goal_for_commit, &report).await;
        }

        for body in &report.expressed {
            writer.write_all(body.as_bytes()).await?;
            writer.write_all(b"\n").await?;
        }
        for (capability, result) in &report.results {
            let rendered = render_result(capability, result);
            if !rendered.is_empty() {
                writer.write_all(rendered.as_bytes()).await?;
                writer.write_all(b"\n").await?;
            }
        }
        for failed in &report.failed {
            writer
                .write_all(format!("[error] capability `{failed}` failed\n").as_bytes())
                .await?;
        }
        writer.flush().await?;
    }

    stream.shutdown();
    Ok(())
}

/// Render a capability's result for the user, when it carries something worth
/// showing. `cap.shell.run` shows its stdout (and stderr); `cap.state.get` shows
/// the value. Effects the provider already narrated (a write, a set) return an
/// empty string here — no double reporting.
fn render_result(capability: &str, result: &Value) -> String {
    if let Some(stdout) = result.get("stdout").and_then(|s| s.as_str()) {
        let mut out = stdout.trim_end().to_string();
        if let Some(stderr) = result.get("stderr").and_then(|s| s.as_str()) {
            let stderr = stderr.trim_end();
            if !stderr.is_empty() {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(stderr);
            }
        }
        return out;
    }
    if capability == "cap.state.get" {
        if let Some(value) = result.get("value") {
            return format!("= {value}");
        }
    }
    String::new()
}

/// Convenience for the binary and tests: run a session over an in-memory byte
/// buffer, returning everything written. Keeps the REPL logic testable without a
/// terminal.
pub async fn run_session_on_bytes(agent: &AssembledAgent, input: &[u8]) -> std::io::Result<String> {
    let mut out: Vec<u8> = Vec::new();
    run_session(agent, BufReader::new(input), &mut out).await?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}
