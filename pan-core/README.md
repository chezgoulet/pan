# pan-core (Wave 0)

The irreducible Pan core: the small, stable Ring 0 that every plugin plugs into.
This crate implements the five core responsibilities from the synthesis as real,
compiling, tested Rust — and, crucially, makes the architecture's central safety
claim *true at the type level* rather than aspirational: **the dangerous path
does not compile.**

> Status: Wave 0 complete. 28 tests pass; three structural bypasses are proven to
> fail compilation. `./verify.sh` checks both halves.

## What this is (and isn't)

The core owns exactly five things (synthesis §2.1): soul handling, admission, the
loop, the dispatch pipeline, and the event stream. Wave 0 builds the substrate for
the last three plus the plugin machinery the others will hang off. There are **no
real plugins here** — only the trivial stubs (`AllowAll`, `EchoExecutor`, the
three demo providers) needed to drive the core end to end.

What is deliberately **absent** from the core: prompts, tokens, models, chat
messages, tool-call conventions. Those live inside `provider.llm` (Wave 1).

## Module → design map

| Module | Design section | What it does |
|---|---|---|
| `schema` | synthesis §2.2, §12 | `Goal` / `ActionIntent` / `Context` / `Capability`. The make-or-break vocabulary. `ActionIntent` has three variants (`Invoke`/`Express`/`Conclude`); state writes are `Invoke` of a state-write capability, never a `Mutate`. |
| `pipeline` | synthesis §3; manifest "the heart" | `resolve → validate → govern → execute → record` as a non-bypassable type-state chain. |
| `loop_engine` | manifest Wave 0 loop | `observe → decide → enact → commit`. Stream-driven; the discrete case is a one-observation span. Houses the abandon-path. |
| `events` | synthesis §6 | Ordered, typed, off-thread event stream. Emit is cheap; a consumer thread does serialization/persistence. |
| `registry` | synthesis §4; Caddy lifecycle | Capability registry + `Register → Provision → Validate → Run → Cleanup`. Conflict = error, never last-wins. |
| `handles` | synthesis §4, §12 | Scoped capability handles. The worked example: a read-only `MemoryQuery` grant that **cannot** write. |
| `providers` | synthesis §2.2; README leak test | The three-provider honesty check (LLM / behavior tree / rules) against one contract. |

## The central invariant, made structural

Boundary #2 says no plugin can skip a pipeline stage — in particular, **execution
cannot happen without a passing govern decision.** This is enforced by types, not
discipline:

```
Resolved --validate--> Validated --govern--> Governed --execute--> Effected
                                                ^
                                  the ONLY source of a Governed
                                  is govern() returning Allow
```

`Governed` has a private field and no public constructor. `Pipeline::execute`
accepts only a `Governed`. Therefore there is no expressible way to execute an
ungoverned effect. The same pattern protects memory: a `MemoryQuery` grant has no
write method, and the concrete handle type is private, so a read grant cannot be
upgraded to a write.

These are not assertions in a comment — they are checked. `tests/compile-fail/`
holds three programs that each attempt a bypass, together with the exact rustc
error they must produce:

- `governed_bypass.rs` → `E0451` (private field) — can't fabricate `Governed`.
- `handle_write.rs` → `E0599` (no method) — can't write through a read handle.
- `handle_downcast.rs` → `E0412` (private type) — can't recover the writer.

If any of these ever compiles, a core boundary has regressed.

## The abandon-path

A `Goal` carries `id` + `revision`. Between *decide* and *enact*, the loop
re-checks supersession; if a newer revision arrived while the provider was
deciding, the whole in-flight decision is discarded **unexecuted**, and the loop
re-decides on the new revision. This is the streaming/voice mechanism, and it is
deliberately the *same* machinery the deferred §14 hardware safety veto will reuse
— building it once means the veto is a matter of *who* sets the flag, not new
plumbing. See `loop_engine::tests::superseded_decision_is_abandoned_not_executed`.

## Run it

```sh
cargo test                  # 28 tests incl. the Wave 0 exit test
cargo run --example walkthrough   # all three providers through one core
./verify.sh                 # tests + the compile-fail guarantees
```

The `walkthrough` example prints the event stream for each provider; you can see
all five pipeline stages firing identically regardless of which provider produced
the decision, and the behavior-tree path emitting no `Express` — the leak check
holding, visibly.

## The Wave 0 exit test

From the build manifest:

> a hand-written integration test drives a stub provider that emits one `Invoke`,
> through an always-allow govern stage, to a stub capability, and sees the event
> on the stream.

That is `tests::wave0_exit_test` in `lib.rs`. It passes. Wave 1 (the first real
provider, `cap.shell`, a CLI channel) can begin.

## License

MIT OR Apache-2.0.
