//! # `provider.command` — a deterministic command interpreter.
//!
//! Maps a user's utterance to capability invokes: `run …` → `cap.shell.run`,
//! `remember …` → `cap.state.set`, `write …` → `cap.fs.write`. It is a real
//! [`Provider`] — no model, no tokens — reinforcing the thesis that the loop is
//! provider-agnostic: an LLM, a rules engine, a behavior tree, and a command
//! parser all emit the same `ActionIntent`s.
//!
//! It lets `pan-agent run` actually *do* things from typed commands, and shows
//! governance in action: an invoke of a capability the persona wasn't granted (or
//! didn't enable) fails at the pipeline and the CLI reports it. The capabilities
//! it targets must be enabled + granted to succeed.

use pan_core::schema::{
    ActionIntent, Capability, Context, Decision, Goal, Outcome, Provider, Trigger, Value,
};

const HELP: &str = "commands: `run <program> [args…]`, `remember <key> <value…>`, `recall <key>`, `write <path> <content…>`";

/// A command-parsing provider.
#[derive(Default)]
pub struct CommandProvider;

impl CommandProvider {
    pub fn new() -> Self {
        Self
    }
}

/// A decision that only speaks (no effect).
fn say(body: impl Into<String>) -> Decision {
    Decision {
        intents: vec![
            ActionIntent::Express { body: body.into() },
            ActionIntent::Conclude {
                outcome: Outcome::Achieved,
            },
        ],
    }
}

/// A decision that speaks, then invokes one capability, then concludes.
fn act(narration: String, capability: &str, args: Value) -> Decision {
    Decision {
        intents: vec![
            ActionIntent::Express { body: narration },
            ActionIntent::Invoke {
                capability: capability.into(),
                args,
                correlation: None,
            },
            ActionIntent::Conclude {
                outcome: Outcome::Achieved,
            },
        ],
    }
}

#[async_trait::async_trait]
impl Provider for CommandProvider {
    fn id(&self) -> &str {
        "provider.command"
    }

    async fn decide(&self, goal: &Goal, _ctx: &Context, _caps: &[Capability]) -> Decision {
        let content = match &goal.trigger {
            Trigger::Utterance { content, .. } => content.trim(),
            _ => return say(HELP),
        };

        let mut parts = content.split_whitespace();
        let command = parts.next().unwrap_or("");
        match command {
            "run" => match parts.next() {
                Some(program) => {
                    let args: Vec<&str> = parts.collect();
                    act(
                        format!("$ {program} {}", args.join(" "))
                            .trim_end()
                            .to_string(),
                        "cap.shell.run",
                        serde_json::json!({ "program": program, "args": args }),
                    )
                }
                None => say("usage: run <program> [args…]"),
            },
            "remember" => match parts.next() {
                Some(key) => {
                    let value = parts.collect::<Vec<_>>().join(" ");
                    act(
                        format!("remembered `{key}`"),
                        "cap.state.set",
                        serde_json::json!({ "key": key, "value": value }),
                    )
                }
                None => say("usage: remember <key> <value…>"),
            },
            "recall" => match parts.next() {
                Some(key) => act(
                    format!("recalling `{key}`:"),
                    "cap.state.get",
                    serde_json::json!({ "key": key }),
                ),
                None => say("usage: recall <key>"),
            },
            "write" => match parts.next() {
                Some(path) => {
                    let text = parts.collect::<Vec<_>>().join(" ");
                    act(
                        format!("wrote `{path}`"),
                        "cap.fs.write",
                        serde_json::json!({ "path": path, "content": text }),
                    )
                }
                None => say("usage: write <path> <content…>"),
            },
            "help" | "" => say(HELP),
            other => say(format!("unknown command `{other}`. {HELP}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utter(text: &str) -> Goal {
        Goal {
            id: "g".into(),
            revision: 0,
            objective: text.into(),
            trigger: Trigger::Utterance {
                from: "user".into(),
                content: text.into(),
            },
        }
    }

    #[tokio::test]
    async fn run_maps_to_a_shell_invoke() {
        let d = CommandProvider::new()
            .decide(&utter("run echo hi there"), &Context::default(), &[])
            .await;
        match &d.intents[1] {
            ActionIntent::Invoke {
                capability, args, ..
            } => {
                assert_eq!(capability, "cap.shell.run");
                assert_eq!(args["program"], "echo");
                assert_eq!(args["args"], serde_json::json!(["hi", "there"]));
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn remember_maps_to_a_state_set() {
        let d = CommandProvider::new()
            .decide(&utter("remember name Sam Vimes"), &Context::default(), &[])
            .await;
        match &d.intents[1] {
            ActionIntent::Invoke {
                capability, args, ..
            } => {
                assert_eq!(capability, "cap.state.set");
                assert_eq!(args["key"], "name");
                assert_eq!(args["value"], "Sam Vimes");
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unknown_command_only_speaks() {
        let d = CommandProvider::new()
            .decide(&utter("frobnicate"), &Context::default(), &[])
            .await;
        assert!(d
            .intents
            .iter()
            .all(|i| !matches!(i, ActionIntent::Invoke { .. })));
    }
}
