use pan_core::loop_engine::RunReport;
use pan_core::schema::{Context, Goal, GoalEval, GoalEvaluator};

/// An LLM-powered goal evaluator.
///
/// After a span concludes `Achieved`, the evaluator sends a lightweight
/// completions request asking whether the goal was actually satisfied. This
/// uses a cheaper/faster model than the main provider (e.g. `llama3.2:1b`
/// vs the agent's `llama3.2:70b`).
///
/// The evaluator sends no tool schemas — it's a pure text-in/text-out check.
pub struct LlmEvaluator {
    base: String,
    model: String,
    api_key: Option<String>,
}

impl LlmEvaluator {
    pub fn new(base: String, model: String, api_key: Option<String>) -> Self {
        Self {
            base,
            model,
            api_key,
        }
    }
}

impl Default for LlmEvaluator {
    fn default() -> Self {
        Self {
            base: "http://127.0.0.1:11434/v1".into(),
            model: "llama3.2:1b".into(),
            api_key: None,
        }
    }
}

#[async_trait::async_trait]
impl GoalEvaluator for LlmEvaluator {
    fn id(&self) -> &str {
        "evaluator.llm"
    }

    async fn evaluate(&self, goal: &Goal, _ctx: &Context, report: &RunReport) -> GoalEval {
        let expressed = report.expressed.join("\n");
        let mut results_text = String::new();
        for (cap, val) in &report.results {
            results_text.push_str(&format!("  {cap}: {val}\n"));
        }

        let prompt = format!(
            "You are a strict goal evaluator. Determine if the goal was SATISFIED or UNSATISFIED.\n\n\
             Goal: {}\n\n\
             Assistant's response:\n{}\n\n\
             Tool results:\n{}\n\n\
             Answer with SATISFIED or UNSATISFIED on the first line, then a brief reason on the second line.\n\
             If you are unsure, answer UNSATISFIED.",
            goal.objective, expressed, results_text
        );

        let messages = vec![serde_json::json!({
            "role": "user",
            "content": prompt
        })];

        let request = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": 64,
            "temperature": 0.1,
        });

        let response = crate::http::post_json_async(
            &self.base,
            "/chat/completions",
            self.api_key.as_deref(),
            &request,
            std::time::Duration::from_secs(15),
        )
        .await;

        match response {
            Ok(body) => {
                let text = body
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|c| c.first())
                    .and_then(|c| c.get("message"))
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_lowercase();

                if text.contains("satisfied") {
                    GoalEval::Satisfied
                } else if text.contains("unsatisfied") {
                    let reason = text.lines().nth(1).unwrap_or("no reason given").to_string();
                    GoalEval::Unsatisfied { reason }
                } else {
                    GoalEval::CannotJudge {
                        reason: format!("unexpected response: {text}"),
                    }
                }
            }
            Err(e) => GoalEval::CannotJudge {
                reason: format!("evaluator request failed: {e}"),
            },
        }
    }
}
