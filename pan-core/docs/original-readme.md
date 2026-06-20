# pan-schema (v1.0)

The `Goal` / `ActionIntent` contract — the make-or-break core vocabulary for Pan.
Reconciled to the complete report's settled design.

This crate settles ONE question: can an LLM provider and a non-LLM provider both
emit the core's decision type *natively*, without either faking the other's
concepts? It answers yes by implementing three providers against the same
contract, in the same file, as an executable leak test.

## Settled design (v1.0)

- `ActionIntent` has THREE variants: `Invoke` (all world-effects, including
  state writes via a state-write capability), `Express` (emit content),
  `Conclude` (span outcome). There is deliberately NO `Mutate` — that was the
  transitional design; state-writes are `Invoke` of `cap.state_write`, gated by
  the pipeline's `govern` stage like any other effect.
- `Goal` carries `id` + `revision`. A new revision supersedes the prior; a
  superseded in-flight `Decision` is discarded at `enact`. Shared abandon-path
  with the deferred hardware safety veto.

## The leak test (why it passes)

1. `ActionIntent` is an enum; `Invoke` is ONE variant, not the whole type.
2. `Invoke.correlation` is `Option` — LLMs set it, BT/rules decline it. A
   required `String` would force non-LLM providers to fabricate ids: a leak.
3. `Express` is "emit to listeners", not "chat". Control-only providers never
   emit it.
4. `Conclude` replaces LLM `stop_reason` with a signal all three produce.

The test `all_three_are_interchangeable_behind_the_trait` holds all three in a
`Vec<Box<dyn Provider>>`. Its compiling IS the thesis.

## Run it

    cargo test
