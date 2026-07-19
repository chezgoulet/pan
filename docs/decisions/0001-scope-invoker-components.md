# ADR 0001 — Scope-aware governance, the ScopedInvoker, and the Component registry

Status: **Accepted** (2026-07-19). Binds the nine-phase extended buildout.
Supersedes the "zero logic changes" framing of Phase 0 in the buildout plan.

## Context

The extended buildout (TUI, GUI, channels, Python skills, self-improvement)
rests on three load-bearing interfaces that the plan scheduled *after* the code
depending on them. A review surfaced three "RED" blockers; grounding them in the
actual Wave-0 source (`pan-core/src/{pipeline,loop_engine,schema,handles,
registry,plugind}.rs`) reveals that two collapse into one root cause, and the
most important issue was not on the list at all:

> **The governor cannot tell who is asking.** `Governor::govern(&self,
> capability: &str, args: &Value) -> Verdict` (`pipeline.rs`) has no notion of
> *origin*. Every safety claim in the plan — per-persona sandboxing, "the
> governor is the sole safety boundary," "the meta-agent cannot modify its own
> thresholds," Python skills with capability-gated I/O — requires distinguishing
> the persona, the skill, and the meta-agent as origins. It structurally cannot.

This ADR fixes the interfaces once, early, so nothing downstream has to be
re-cut later.

## Decisions

### D1 — `govern` becomes origin-aware via a `Scope`

The `Governor` contract gains a `Scope` — the identity and authority of whoever
is driving the invocation:

```rust
fn govern(&self, scope: &Scope, capability: &str, args: &Value) -> Verdict;
```

`Scope` lives in the core vocabulary and carries **identity, not policy**. The
core guarantees only that *every* dispatch carries a `Scope`; what a scope
*permits* is a governor component's business. This preserves the Wave-0
discipline (`pipeline.rs`: "the correctness of a govern policy … is not
[type-enforced]") — the core threads *who*, the governor decides *whether*.

`EffectRequest` carries the scope, so every construction site must answer "on
whose authority?" The loop stamps the persona's scope; a skill's
[ScopedInvoker](#d2--the-scopedinvoker-a-governed-capability-handle) stamps its
own narrower scope.

Four plan features reduce to this one change:
- Per-persona `cap.shell`/`cap.http`/`cap.fs` boundaries (Phase 5).
- Nested skill invokes governed against the *skill's* sub-scope, not the
  persona's full grant.
- The meta-agent forbidden from editing its own config (Phase 7) — govern sees
  `origin = meta.self-improve`, target = its own section, denies.
- "One safety layer, no bypass" becomes *true* rather than aspirational.

A reusable `ScopedGovernor` (origin → allowed-capability-prefix map, the Phase-5
shape) ships alongside `AllowAll` to demonstrate and test enforcement now.

### D2 — the `ScopedInvoker`, a governed capability handle

A Python skill calling `cap.invoke("cap.fs.read", …)` does **not** fit
`Executor::execute(capability, args) -> Result<Value>` — that trait is a *leaf*
(terminal effect, no further invocation). Modeling a skill as an Executor is the
category error behind "RED #2." A skill that emits invokes is behaving like a
**provider of `ActionIntent::Invoke`s that must run through the full pipeline.**

The fix reuses the pattern already proven in `handles.rs` (`MemoryQuery`: a
read-only surface whose writer is structurally unreachable). We add an
invocation analogue:

```rust
// The ONLY surface a skill/sub-agent holds to reach the outside world.
trait ScopedInvoker {
    fn invoke(&self, capability: &str, args: &Value) -> Result<Value, InvokeError>;
}
```

Internally `invoke` calls `pipeline.dispatch` — the **full** `resolve → validate
→ govern → execute` chain — carrying the handle's bound `Scope`. The subprocess
bridge is then *only a transport*: JSON-lines over stdin/stdout that turns a
subprocess "invoke" message into one `ScopedInvoker::invoke` Rust call and
streams the result back. The subprocess holds **zero ambient authority** (no fs,
no net); its only channel to the world is that protocol → the invoker → the
pipeline.

**The `Governed` invariant is preserved verbatim.** The subprocess holds no Rust
objects and cannot fabricate a `Governed` (`pipeline.rs`: private field, no
public constructor); only `govern()` returning `Allow` yields one. A
`tests/compile-fail/` guard asserts the bridge exposes nothing but `invoke`,
mirroring `handle_write.rs`.

Reentrancy (pipeline → execute → skill → pipeline) is handled by giving the
invoker `Arc`-shared, `Send + Sync` access to the pipeline services rather than a
back-`&`-reference. Once the loop is async (D4), a skill blocked awaiting an
invoke result is a *suspended future*, not a blocked thread — the OS process is
per-skill, the Rust side just awaits.

### D3 — "Component" vs "Plugin": name the two mechanisms, build the registry

Two distinct extension mechanisms coexist and must be named permanently to end
the terminology collision:

- **Component** — an in-process trait impl in one of the families (`Provider`,
  `Executor`, `Governor`, `Channel`, `ContextSource`, scheduler `Condition`),
  selected and wired by `Agent.toml` through a **`ComponentRegistry`** factory
  (config id → constructor). *This is what Phases 2–8 build.* It replaces the
  hard-coding in `pan-daemon/src/session.rs` (`RulesProvider` + `AllowAll` +
  `EchoExecutor` today).
- **Plugin** — the out-of-process Wasm/`plugind` mechanism
  (`plugind.rs`, `provision/validate/run/cleanup`, currently `#62` stubs).
  Orthogonal, later, off the critical path.

The `ComponentRegistry` is the wiring backbone that makes personas and config
real; build it in Phase 2.

### D4 — the abandon-path is made concurrent (a real logic change)

The async conversion of `loop_engine.rs` is **not** "zero logic changes." Today
the loop is *sequential*: `decide()` runs to completion, *then* supersession is
checked. That compiles fine async (the decide future's borrow of `current` ends
before `superseding` is called), but it does not deliver the feature the
abandon-path exists for — "cancel the in-flight decision the moment a newer
revision arrives" (streaming/voice). A 5-second async `decide` still finishes
before a revision that arrived at second 1 is noticed.

Phase 0 restructures decide-vs-supersession into a concurrent race that **drops
(cancels)** the decide future when a newer revision arrives, with `decide` taking
an owned `Arc<Goal>` so cancellation-and-reassignment is borrow-clean. New test:
*a supersession arriving mid-decide cancels the decide future before it
completes.* The type-state invariant and the supersession predicate
(`Goal::superseded_by`) are unchanged.

## Reuse (do not duplicate)

1. **`gov.audit` is an `EventStream` sink, not a new system.** The pipeline
   already emits `StageCompleted`/`Effected`/`Denied` through `EventStream` with
   a swappable `Sink` (`events.rs`). Durable audit = an NDJSON/SQLite sink over
   that existing stream, which makes it non-bypassable for free.
2. **Live config apply = the `Arc`-swap already written.** `plugind.rs` reloads
   via atomic `Arc<PluginSet>` swap; the self-improvement loop's approved config
   changes reuse that pattern for the component graph.
3. **The Soul Protocol daemon is a channel, not a casualty.** Phase 1 frames the
   daemon as `channel.soul-protocol` over the unchanged core; the 15-fixture
   conformance suite and the REACHLOCK integration harness (`ci.yml`) stay green.

## Invariants that must not regress

- Execution requires a `Governed`, whose only source is `govern() == Allow`
  (the three `tests/compile-fail/` guards keep compiling-must-fail).
- The three-provider leak test (`providers.rs`): every new provider/skill passes
  an interchangeability check against the same `ActionIntent` vocabulary.
- No policy in the core. `Scope` carries identity; grants live in governor
  config (`Agent.toml [caps.grant]`).
- The self-improvement loop cannot auto-approve a scope escalation (policy- or
  type-guarded).

## Corrected build order (load-bearing first)

1. `Scope` on `govern` + `ScopedGovernor` (this ADR; Phase 0/2). ← everything
   governed/sandboxed/self-improving depends on it.
2. `ScopedInvoker` handle + minimal subprocess transport + compile-fail guard.
3. `ComponentRegistry` factory; retire `session.rs` hard-coding (Phase 2).
4. Concurrent cancellable abandon-path with `Arc<Goal>` (Phase 0, async).

The remaining phases sit on the Wave-0 type-state pipeline unchanged.

## Implementation status

Landed (this pass — synchronous, all guarantees green, 96 workspace tests):

- **D1 — `Scope` on `govern`.** `schema::Scope`; `Governor::govern(&Scope, …)`;
  `EffectRequest.scope`; the loop stamps `Loop.scope`; the daemon stamps
  `soul.<id>`. A reusable `pipeline::ScopedGovernor` (origin → prefix grants,
  deny-by-default) demonstrates per-origin sandboxing, with an end-to-end test
  that the same capability is allowed for a granted origin and denied for
  another. The `governed_bypass` compile-fail guard still rejects with **E0451**.
- **D2 — `ScopedInvoker`.** New `invoker` module: `ScopedInvoker` trait,
  `InvokeError`, `PipelineInvoker` (routes through the full pipeline under a bound
  scope), and `sub()` for narrower nested origins. Tests prove a stand-in "skill"
  that holds only `&dyn ScopedInvoker` is governed by its bound scope and cannot
  escalate. New compile-fail guard `invoker_no_scope_injection` rejects a
  scope-injecting `invoke` call with **E0061** — "a skill cannot widen its own
  authority," structurally.
- **D3 — `ComponentRegistry`.** New `components` module: per-family factory tables
  (`Provider`/`Governor`/`Executor`), conflict-is-error registration, config-slice
  construction, tests wiring built components into a real pipeline.
- **Tooling.** `verify.sh` now links the rlib from the actual (workspace) target
  instead of a stale `pan-core/target`, and treats rustc error-code drift as a
  warning while still failing on a bypass that compiles or a setup with no
  compiler error at all.

Pending (next):

- **D4 — concurrent cancellable abandon-path.** Requires the async refactor
  (tokio, `async-trait`); deliberately not started here to avoid a half-migrated
  tree. `decide` takes an owned `Arc<Goal>`; decide is raced against a
  supersession signal and dropped on supersession. New test: supersession
  mid-decide cancels the decide future before it completes.
- **Retire the daemon's hard-coded wiring** onto `ComponentRegistry`. Note the
  daemon's `ResolveGovernor<'a>` borrows the capability registry, so this is a
  real lifetime restructuring (build components into session-owned storage), not
  a mechanical swap — Phase 2 work, done with care.
- **Subprocess transport** for `ScopedInvoker` (JSON-lines over stdin/stdout to
  `python3`), landing on the async story so a blocked skill is a suspended future.
