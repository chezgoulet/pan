use std::sync::RwLock;

use pan_core::loop_engine::RunReport;
use pan_core::schema::{Context, ContextAssembler, Goal, Trigger, Value};

/// Rolling conversation history assembler.
///
/// Keeps the last N turns (user + assistant) in memory and injects them
/// as a `history` channel fragment on each goal. The LLM provider
/// (`provider.llm` / `provider.anthropic`) replays these as prior
/// `user`/`assistant` messages between the system prompt and the current
/// user turn.
pub struct RollingConversationHistory {
    max_turns: usize,
    history: RwLock<Vec<Value>>,
}

impl RollingConversationHistory {
    pub fn new(max_turns: usize) -> Self {
        Self {
            max_turns,
            history: RwLock::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl ContextAssembler for RollingConversationHistory {
    fn id(&self) -> &str {
        "context.rolling_history"
    }

    async fn assemble(&self, _goal: &Goal) -> Context {
        let history = self.history.read().unwrap();
        if history.is_empty() {
            return Context::default();
        }
        let body = serde_json::to_string(&*history).unwrap();
        Context::default().with("history", body)
    }

    async fn commit(&self, goal: &Goal, report: &RunReport) {
        let user_content = match &goal.trigger {
            Trigger::Utterance { content, .. } => content.clone(),
            _ => goal.objective.clone(),
        };
        let mut history = self.history.write().unwrap();
        history.push(serde_json::json!({"role": "user", "content": user_content}));
        for body in &report.expressed {
            if !body.is_empty() {
                history.push(serde_json::json!({"role": "assistant", "content": body}));
            }
        }
        while history.len() > self.max_turns * 2 {
            history.remove(0);
        }
    }
}

/// Builder for use with [`ComponentRegistry`].
fn build_rolling_history(
    cfg: &pan_core::components::ComponentConfig,
) -> Result<Box<dyn ContextAssembler>, pan_core::components::ComponentError> {
    let max_turns = cfg
        .settings
        .get("max_turns")
        .and_then(|v| v.as_u64())
        .unwrap_or(20) as usize;
    Ok(Box::new(RollingConversationHistory::new(max_turns)))
}

/// Register the built-in context assemblers into a registry.
pub fn register_assemblers(registry: &mut pan_core::components::ComponentRegistry) {
    registry
        .register_context_assembler("context.rolling_history", build_rolling_history)
        .expect("register context.rolling_history");
}
