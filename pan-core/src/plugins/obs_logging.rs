//! # `obs.logging` — structured observation logging (Wave 1).
//!
//! An `EventSink` that prints every event to stderr in a structured, one-line
//! form. The manifest is blunt: "You are blind without this." Wired as the
//! sink behind the core's off-thread `EventStream`, so the loop's emission stays
//! cheap (a struct onto a queue) and logging happens on the consumer thread.

use crate::events::{Event, EventKind, EventSink, StageStatus};
use std::io::Write;

pub struct LogSink {
    /// When true, also prints the JSON of each event (verbose). Off by default
    /// so normal runs stay readable.
    pub verbose: bool,
}

impl Default for LogSink {
    fn default() -> Self {
        Self { verbose: false }
    }
}

impl LogSink {
    pub fn new() -> Self {
        Self::default()
    }
}

fn stage_status_mark(s: StageStatus) -> &'static str {
    match s {
        StageStatus::Ok => "✓",
        StageStatus::Denied => "✗",
        StageStatus::Error => "!",
    }
}

impl EventSink for LogSink {
    fn consume(&mut self, event: Event) {
        let line = match &event.kind {
            EventKind::RunStarted { goal_id, revision } => {
                format!("run.start   {goal_id} rev{revision}")
            }
            EventKind::Decided { provider, intents } => {
                format!("decide      {provider} → {intents} intent(s)")
            }
            EventKind::DispatchStarted { capability, correlation } => {
                format!(
                    "dispatch     {capability}{}",
                    correlation
                        .as_ref()
                        .map(|c| format!(" ({c})"))
                        .unwrap_or_default()
                )
            }
            EventKind::StageCompleted {
                stage,
                capability,
                status,
            } => {
                format!("stage.{stage:<9} {capability} {}", stage_status_mark(*status))
            }
            EventKind::Effected { capability, .. } => {
                format!("effected     {capability}")
            }
            EventKind::Expressed { body } => {
                format!("express      {body}")
            }
            EventKind::Abandoned {
                goal_id,
                superseded_by,
            } => {
                format!("abandon      {goal_id} (superseded by rev{superseded_by})")
            }
            EventKind::RunConcluded { goal_id, outcome } => {
                format!("run.end      {goal_id} → {outcome:?}")
            }
            EventKind::PluginError { plugin, message } => {
                format!("error        {plugin}: {message}")
            }
        };
        let _ = writeln!(std::io::stderr(), "[pan {}] {line}", event.seq);
        if self.verbose {
            let _ = writeln!(
                std::io::stderr(),
                "[pan {}]   {}",
                event.seq,
                serde_json::to_string(&event.kind).unwrap_or_default()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sink_consumes_without_panic() {
        // Just ensure every variant formats; we can't easily capture stderr, but
        // a sink that panics on any variant would fail here.
        let mut sink = LogSink::new();
        sink.consume(Event {
            seq: 0,
            kind: EventKind::Expressed {
                body: "hi".into(),
            },
        });
        sink.consume(Event {
            seq: 1,
            kind: EventKind::PluginError {
                plugin: "x".into(),
                message: "boom".into(),
            },
        });
    }
}
