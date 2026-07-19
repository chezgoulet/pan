//! # `provider.echo` — a dependency-free conversational provider.
//!
//! The stock providers that react to *events* and *signals* (rules) or tick a
//! tree (behavior tree) don't answer an utterance, and the real LLM provider
//! needs an endpoint. `EchoProvider` fills the gap: it replies to what the user
//! said, deterministically and with no dependency. It exists so `pan-agent run`
//! is interactive out of the box, and so the utterance → Express path has a
//! provider to exercise — it is a real [`Provider`], not a special case.

use pan_core::schema::{
    ActionIntent, Capability, Context, Decision, Goal, Outcome, Provider, Trigger,
};

/// A provider that echoes the user's utterance back as an `Express`, then
/// concludes. A configurable `prefix` (from `[persona] prefix = "…"`) lets an
/// `Agent.toml` shape the reply.
pub struct EchoProvider {
    pub prefix: String,
}

impl Default for EchoProvider {
    fn default() -> Self {
        Self {
            prefix: "you said".into(),
        }
    }
}

#[async_trait::async_trait]
impl Provider for EchoProvider {
    fn id(&self) -> &str {
        "provider.echo"
    }

    async fn decide(&self, goal: &Goal, _ctx: &Context, _caps: &[Capability]) -> Decision {
        let body = match &goal.trigger {
            Trigger::Utterance { content, .. } => format!("{}: {content}", self.prefix),
            Trigger::Event { topic, .. } => format!("{}: (event {topic})", self.prefix),
            Trigger::Signal { name, value } => format!("{}: (signal {name}={value})", self.prefix),
            Trigger::Tick { sequence } => format!("{}: (tick {sequence})", self.prefix),
        };
        Decision {
            intents: vec![
                ActionIntent::Express { body },
                ActionIntent::Conclude {
                    outcome: Outcome::Achieved,
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn echoes_an_utterance() {
        let p = EchoProvider::default();
        let goal = Goal {
            id: "g".into(),
            revision: 0,
            objective: "greet".into(),
            trigger: Trigger::Utterance {
                from: "user".into(),
                content: "hello there".into(),
            },
        };
        let d = p.decide(&goal, &Context::default(), &[]).await;
        match &d.intents[0] {
            ActionIntent::Express { body } => assert_eq!(body, "you said: hello there"),
            other => panic!("expected Express, got {other:?}"),
        }
    }
}
