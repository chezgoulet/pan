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
- **D4 — async core with a real cancellable abandon-path.** `Provider`,
  `Governor`, `Executor`, `Observations`, and `ScopedInvoker` are async (via
  `async-trait`, for dyn-compatibility); `Pipeline::{govern,execute,dispatch}`
  and `Loop::run_span` are async. The abandon-path is now a `tokio::select!`
  (`biased`) race between `decide` and `Observations::superseded`: a newer
  revision arriving mid-decide **drops** the in-flight decide future. Both racing
  futures borrow a per-iteration `snapshot`, never `current`, so the supersession
  arm reassigns `current` cleanly. New test
  `supersession_mid_decide_cancels_the_decide_future` proves cancellation by
  counting *completed* decides: exactly one (the survivor), not two. The daemon
  stays thread-per-perceive and bridges via `pan_daemon::block_on` at its two
  async seams (`decide`, `dispatch_decision`); Soul Protocol conformance (19)
  unchanged.
- **The subprocess transport — a working Python skill runtime.** New crate
  `pan-skill` (kept out of the irreducible core: the subprocess runtime is a
  component, not core). `SkillRunner` spawns a skill as a `python3` subprocess and
  services each capability it invokes through a `ScopedInvoker` — the full
  governed pipeline, under the skill's bound scope. The subprocess holds no Pan
  capability object; its only sanctioned channel is a newline-JSON invoke ↔ result
  protocol over stdin/stdout, and it `import pan` (an embedded ~1-page client).
  Async throughout, `kill_on_drop`, stderr captured for tracebacks. Four
  end-to-end tests spawn real `python3`, including the crux: an out-of-scope
  invoke surfaces as `PanDenied` **inside the subprocess** — governance crosses
  the process boundary. Honest scope: this guarantees all *sanctioned* I/O is
  governed; OS-level denial of *ambient* fs/network (namespaces/seccomp/`bwrap`)
  plugs into `SkillRunner::with_program` and is not yet enforced.

- **The config model — `Agent.toml` + assembler (Design Decision #1).** New crate
  `pan-agent`: `AgentManifest` parses the one-file-per-instance manifest, and
  `assemble` turns it into an `AssembledAgent` — the persona's `Scope`, a
  `ScopedGovernor` built from `[caps.grant]` (each `family = true` grants
  `cap.<family>`, deny-by-default), and the provider built through a
  `ComponentRegistry`. This makes D1 (Scope) and D3 (ComponentRegistry) *real from
  config* rather than hand-wired: an end-to-end test shows `shell = true` / `fs =
  false` in TOML gating an actual dispatch. A persona is now one declared concept
  (authority + voice + brain); an unknown `persona.provider` is a load-time error.

- **The executor/capability model — `Toolbox` + concrete `cap.*` components.**
  pan-core gains `CapabilityProvider` (a component that declares + executes
  capabilities) and `Toolbox` (the plan's `exec.local`: it composes many
  providers, builds the merged `CapabilityRegistry` the pipeline resolves against,
  and *is* the `Executor`, routing each capability to its owner; collisions are
  conflict errors). New crate `pan-cap` supplies real components: `cap.state` (an
  in-memory KV) and `cap.fs` (rooted file access, with executor-level path jailing
  as defense in depth). This is the missing link between an *assembled* agent and
  a *doing* agent: end-to-end tests drive a provider → loop → govern → real
  `cap.fs.write`, writing an actual file, while an ungranted origin is denied at
  govern and the file is left untouched, and a granted persona still cannot escape
  its fs root.

- **The arc closed — `Agent.toml` → a fully runnable agent.** `ComponentRegistry`
  gained a capability-provider factory family; `pan-cap` registers its components
  (`register_builtin_caps`); the manifest grew `[caps.enable]` (which components
  exist), `[caps.settings."cap.x"]` (per-component config, e.g. `cap.fs`'s root),
  and pass-through `[persona]` settings (a provider's own config, e.g. a rules
  array). `assemble` now also builds the persona's `Toolbox`, so an
  `AssembledAgent` carries *everything a loop needs* — scope, governor, provider,
  and toolbox (registry + executor). The capstone test proves it: one Agent.toml
  (a rules brain + enabled, rooted `cap.fs` + an `fs` grant) assembles and drives
  one loop span that writes a **real file** — config to running agent, no
  hand-wiring. Enabling an unknown capability, or `cap.fs` without a root, is a
  load-time error.

- **A runnable interactive agent — `pan-agent run`.** New crate `pan-cli`:
  `run_session` drives a REPL over async byte streams (each line → an `Utterance`
  goal → one governed loop span → the provider's `Express` written back), and the
  `pan-agent` binary is a thin `main` over it on stdin/stdout. A dependency-free
  `provider.echo` (in pan-agent) makes it conversational out of the box, so the
  utterance → Express path has a real provider to exercise. The binary runs live
  (`printf 'hello\n/quit\n' | pan-agent run Agent.toml` → `echo: hello`), and the
  REPL is tested end-to-end over in-memory buffers. The harness is
  provider-agnostic — swap in a rules brain or a real LLM and only the brain
  changes; every effect still flows through the governed pipeline.

- **Interactive capabilities — the agent does real, governed work.** `cap.shell`
  (run a program *directly* — no shell, so no injection class; exit/stdout/stderr
  returned) joins `pan-cap`. `provider.command` (in pan-agent) is a deterministic
  interpreter mapping utterances to invokes (`run`/`remember`/`recall`/`write` →
  `cap.shell`/`cap.state`/`cap.fs`) — a fifth provider kind that reinforces
  "many providers, one contract". `RunReport` gained an additive `results` field
  (each effect's return value, surfaced synchronously — no racing the off-thread
  event stream), and the CLI renders it (shell stdout, recalled values). Live:
  `run echo …` / `remember`/`recall` / `run uname -s` all work through
  `pan-agent run`, governed — and an enabled-but-ungranted `cap.shell` is denied
  at `govern` and reported. `cap.shell`'s arg-level policy (a program allowlist)
  is a future governor concern; today the boundary is the persona's grant.

- **The agentic tool-use (ReAct) loop — a provider can now *use* a tool, not just
  name one.** Until now every provider concluded in a single decision: it could
  emit an `Invoke`, but never see the result. `Loop::run_span` gained a second,
  inner loop: when a decision **acts without concluding**, each executed effect —
  success *or* denial/error — is folded back into a per-goal working context as a
  fragment on `TOOL_RESULT_CHANNEL` (`{capability, correlation?, result|error}`,
  opaque to the core), and the provider re-decides on the **same** goal with the
  results in hand. It loops until the provider `Conclude`s, bounded by
  `MAX_TOOL_STEPS` (a runaway ends the span as the new `RunEnd::StepLimit`, so the
  loop always terminates). Fully backward-compatible: every existing provider
  concludes in one step and never enters the inner loop; the abandon-path is
  unchanged (a superseding revision still drops the in-flight decide and restarts
  with a fresh working context). Two new tests prove it — a ReAct provider that
  invokes, sees its own `correlation` + result fed back, then answers and
  concludes (decided exactly twice, effect fired once); and a never-concluding
  provider stopped precisely at the step cap. This is the keystone the LLM
  provider plugs into: it makes tool-*using* intelligence possible without any
  provider being privileged, since the feedback rides the same opaque `Context`
  fragments a rules/BT provider simply ignores. The fragment body records the
  whole exchange (`{capability, correlation?, args, result|error}`) — carrying
  `args` so a *stateless* tool-using provider can rebuild the assistant turn, not
  just the result.

- **A tool-using LLM brain — `provider.llm` (new crate `pan-llm`).** The payoff
  of the ReAct loop: a real model that *uses* tools, as an ordinary `Provider`
  (no chat-shaped types leak into the core). It maps the agent's capabilities to
  the OpenAI function schema (`cap.state.get` → `cap_state_get`, mapped back on
  the way in), turns a model `tool_calls` reply into governed `Invoke`s (tool_call
  id → `correlation`, **no `Conclude`** so the loop continues), and reads the
  executed results back off `TOOL_RESULT_CHANNEL`. It is **stateless**: each
  `decide` reconstructs the full function-calling transcript (system, user, then
  each `assistant(tool_call)` → `tool(result)` pair) from the goal + fragments, so
  a cancelled decide leaves nothing behind. Transport is a tiny std-only blocking
  HTTP/1.0 client (`pan-llm::http`) that follows the `base` scheme: plain
  `TcpStream` for local servers (`http://` — Ollama, llama.cpp, LM Studio), and a
  **rustls TLS** stream for cloud BYOK (`https://` — OpenAI, OpenRouter, Groq,
  Together, an Anthropic-compatible endpoint), pure-Rust via the `ring` provider +
  `webpki-roots` (no cmake/C toolchain, no system cert store). Registered into
  `pan-agent`'s builtin set,
  so any `Agent.toml` selects `provider = "provider.llm"` (with `base`/`model`,
  falling back to `PAN_LLM_*`; a missing endpoint is a load-time error). Tests run
  with **no network or key**: unit tests cover schema mapping, transcript
  reconstruction, and response interpretation, and `tests/tool_use.rs` drives the
  *whole* ReAct cycle — model asks for a tool, the loop executes the governed
  capability, the model sees the result and answers — against a localhost mock,
  asserting the second request replays the tool_call and its result. The `https`
  (TLS) path is exercised live by `tests/live_cloud.rs`, credential-gated on
  `PAN_LLM_*` so CI/offline skip it; the TLS wiring itself (ring provider + root
  store) is unit-tested with no network.

Pending (next):

- **OS-level skill sandbox** — wire `SkillRunner::with_program` to a real sandbox
  launcher (`bwrap`/`nsjail` or namespaces + seccomp) so a skill's *ambient*
  syscalls are denied, not just its unsanctioned Pan calls.
- **Fully async daemon** — drop the `block_on` bridge: convert `server.rs` (TCP
  loopback) and `session.rs` to tokio, and give `llm.rs` a non-blocking client.
  Only then does one slow soul stop occupying a whole OS thread.
- **Retire the daemon's hard-coded wiring** onto `ComponentRegistry`. Note the
  daemon's `ResolveGovernor<'a>` borrows the capability registry, so this is a
  real lifetime restructuring (build components into session-owned storage), not
  a mechanical swap — Phase 2 work, done with care.
- **`skill.*` lifecycle capabilities** (`skill.create/edit/list/delete`) wrapping
  the runner — the Phase-7 management surface, itself governed.
