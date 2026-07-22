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
    registry
        .register_context_assembler("context.memory_retrieval", build_memory_retrieval)
        .expect("register context.memory_retrieval");
}

/// Memory retrieval assembler: reads `cap.state`'s persisted JSON file and
/// injects matching facts as `memory` channel fragments. Uses simple text
/// matching — a future vector store would do semantic retrieval.
pub struct MemoryRetrievalAssembler {
    state_path: std::path::PathBuf,
}

impl MemoryRetrievalAssembler {
    pub fn new(state_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            state_path: state_path.into(),
        }
    }

    fn read_state(&self) -> Vec<(String, String)> {
        let text = match std::fs::read_to_string(&self.state_path) {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        let map: std::collections::HashMap<String, pan_core::schema::Value> =
            match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(_) => return vec![],
            };
        map.into_iter().map(|(k, v)| (k, v.to_string())).collect()
    }
}

#[async_trait::async_trait]
impl ContextAssembler for MemoryRetrievalAssembler {
    fn id(&self) -> &str {
        "context.memory_retrieval"
    }

    async fn assemble(&self, goal: &pan_core::schema::Goal) -> pan_core::schema::Context {
        let facts = self.read_state();
        let query = goal.objective.to_lowercase();
        let mut ctx = pan_core::schema::Context::default();
        for (key, value) in &facts {
            let text = format!("{key} {value}").to_lowercase();
            if text.contains(&query) || query.contains(&key.to_lowercase()) {
                ctx = ctx.with("memory", format!("{key}: {value}"));
            }
        }
        ctx
    }
}

/// Builder for `context.memory_retrieval`.
fn build_memory_retrieval(
    cfg: &pan_core::components::ComponentConfig,
) -> Result<Box<dyn ContextAssembler>, pan_core::components::ComponentError> {
    let path = cfg
        .settings
        .get("state_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| pan_core::components::ComponentError::Construction {
            id: cfg.id.clone(),
            reason: "context.memory_retrieval requires `state_path` setting (path to cap.state's JSON file)".into(),
        })?;
    Ok(Box::new(MemoryRetrievalAssembler::new(path)))
}
