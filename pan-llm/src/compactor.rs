use pan_core::schema::{Context, ContextBudget, ContextCompactor};

/// A compactor that drops the oldest non-system fragments until the
/// estimated token count fits within the budget.
pub struct TruncationCompactor;

impl Default for TruncationCompactor {
    fn default() -> Self {
        Self
    }
}

impl TruncationCompactor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl ContextCompactor for TruncationCompactor {
    fn id(&self) -> &str {
        "compactor.truncation"
    }

    async fn compact(&self, ctx: &Context, budget: &ContextBudget) -> Context {
        if ContextBudget::estimate_tokens(ctx) <= budget.max_tokens {
            return ctx.clone();
        }

        let mut compacted = ctx.clone();
        // Sort fragments: keep system and objective fragments, drop oldest
        // tool_result and history fragments first.
        compacted.fragments.retain(|f| {
            f.channel == "system" || f.channel == "objective" || f.channel == "persona"
        });
        // Re-add non-essential fragments (tool_result, history, memory, etc.)
        // from back to front (most recent first) until budget is nearly full.
        let mut tail: Vec<_> = ctx
            .fragments
            .iter()
            .filter(|f| f.channel != "system" && f.channel != "objective" && f.channel != "persona")
            .collect();

        // Most recent last.
        tail.reverse();
        for f in tail {
            if ContextBudget::estimate_tokens(&compacted) + f.body.len() / 4 <= budget.max_tokens {
                compacted.fragments.push(f.clone());
            }
        }

        // If even the minimal context exceeds budget, just return it as-is.
        compacted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pan_core::schema::Fragment;

    #[tokio::test]
    async fn truncation_compactor_keeps_within_budget() {
        let compactor = TruncationCompactor::new();
        let mut ctx = Context::default();
        ctx.fragments.push(Fragment {
            channel: "system".into(),
            body: "you are a helpful agent".into(),
        });
        // Add 10 large tool_result fragments (200 chars each ≈ 50 tokens).
        for i in 0..10 {
            ctx.fragments.push(Fragment {
                channel: "tool_result".into(),
                body: format!("very long tool result with lots of data for iteration number {i} and some more padding to make it big"), // ~120 chars
            });
        }

        let budget = ContextBudget { max_tokens: 50 }; // allow ~50 tokens = ~200 chars
        let compacted = compactor.compact(&ctx, &budget).await;

        assert!(
            ContextBudget::estimate_tokens(&compacted) <= 50,
            "compacted context must fit within budget ({} tokens)",
            ContextBudget::estimate_tokens(&compacted)
        );
        // System prompt must be preserved.
        assert!(compacted.fragments.iter().any(|f| f.channel == "system"));
    }

    #[tokio::test]
    async fn truncation_compactor_preserves_system_and_objective() {
        let compactor = TruncationCompactor::new();
        let mut ctx = Context::default();
        ctx.fragments.push(Fragment {
            channel: "system".into(),
            body: "system".into(),
        });
        ctx.fragments.push(Fragment {
            channel: "objective".into(),
            body: "do x".into(),
        });
        ctx.fragments.push(Fragment {
            channel: "history".into(),
            body: "a".repeat(400),
        }); // ~100 tokens
        ctx.fragments.push(Fragment {
            channel: "tool_result".into(),
            body: "b".repeat(400),
        }); // ~100 tokens

        let budget = ContextBudget { max_tokens: 30 };
        let compacted = compactor.compact(&ctx, &budget).await;

        // System and objective must survive.
        assert_eq!(
            compacted
                .fragments
                .iter()
                .filter(|f| f.channel == "system" || f.channel == "objective")
                .count(),
            2
        );
    }
}
