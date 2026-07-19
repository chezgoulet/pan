//! Live, credential-gated smoke test of the real TLS transport against a cloud
//! (or any real) OpenAI-compatible endpoint. It is a **no-op unless** all three
//! of `PAN_LLM_BASE`, `PAN_LLM_MODEL`, and `PAN_LLM_API_KEY` are set, so CI and
//! offline runs skip it cleanly. To exercise the `https://` path for real:
//!
//! ```sh
//! PAN_LLM_BASE=https://api.openai.com/v1 \
//! PAN_LLM_MODEL=gpt-4o-mini \
//! PAN_LLM_API_KEY=sk-... \
//! cargo test -p pan-llm --test live_cloud -- --nocapture
//! ```
//!
//! The deterministic behavior of the provider is covered offline by
//! `tests/tool_use.rs` (a localhost mock); this only checks that a genuine TLS
//! round-trip reaches a model and comes back as a spoken answer.

use pan_core::schema::{Context, Goal, Outcome, Provider, Trigger};
use pan_llm::OpenAiProvider;

#[tokio::test]
async fn a_real_endpoint_answers_over_tls() {
    let (Ok(base), Ok(model), Ok(api_key)) = (
        std::env::var("PAN_LLM_BASE"),
        std::env::var("PAN_LLM_MODEL"),
        std::env::var("PAN_LLM_API_KEY"),
    ) else {
        eprintln!("live_cloud: PAN_LLM_BASE / PAN_LLM_MODEL / PAN_LLM_API_KEY unset — skipping");
        return;
    };

    let provider = OpenAiProvider {
        base,
        model,
        api_key: Some(api_key),
        instruction: "You are a terse assistant. Answer in one short sentence.".into(),
        max_tokens: 64,
        temperature: 0.0,
    };

    let goal = Goal {
        id: "live".into(),
        revision: 0,
        objective: "Answer the user.".into(),
        trigger: Trigger::Utterance {
            from: "user".into(),
            content: "Say hello in exactly three words.".into(),
        },
    };

    // No tools this turn: a plain answer should come straight back as
    // Express + Conclude(Achieved).
    let decision = provider.decide(&goal, &Context::default(), &[]).await;
    assert_ne!(
        decision.outcome(),
        Some(Outcome::Abandoned),
        "the endpoint should answer, not abandon (check base/model/key and connectivity)"
    );
    let spoken = decision.intents.iter().find_map(|i| match i {
        pan_core::schema::ActionIntent::Express { body } => Some(body.clone()),
        _ => None,
    });
    eprintln!("live_cloud: model said: {spoken:?}");
    assert!(spoken.is_some(), "expected a spoken answer");
}
