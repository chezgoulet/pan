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
