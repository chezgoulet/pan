# Handoff — Pan extended buildout (ADR 0001)

_Living continuity doc. Update the "Status" and "What's next" sections as work
lands. The authoritative design is [ADR 0001](decisions/0001-scope-invoker-components.md);
the always-loaded orientation is [`/CLAUDE.md`](../CLAUDE.md). Read both first._

## Status (branch `testing`, pushed to `origin/testing`)

Pan is a **runnable, governed, interactive, tool-using agent assembled from
`Agent.toml`** — with a real LLM brain (`provider.llm`) that *uses* tools — plus
an OpenAI-compatible HTTP gateway, a Python skill runtime, and the Soul Protocol
daemon. Everything below is **green**: 249 tests, workspace `fmt` + `clippy -D
warnings` clean, the four `pan-core` compile-fail guards hold, and Soul Protocol
conformance is 19/19.

All ROADMAP items — including the six deferred phases — are landed. The remaining
gaps are documentation polish, integration test breadth, and TUI real-time streaming.

This effort added these commits on top of `f16fd15`: 

```
2e43135 Phase F: Lifecycle hooks + path-scoped rules + policy chain
2534c1f Phase E: cap.lsp — language diagnostics capability
16713c5 Phase D: GoalEvaluator — post-span satisfaction check
d9bb5ee Phase C: ContextBudget + Compactor (TruncationCompactor)
3b55f4e Phase B: SessionStore — persistent conversation store
5b23500 Phase A: SnapshotStore + TUI /undo
17a9f5e cap.format — auto code formatter capability
f566aa5 Memory retrieval assembler + StallDetector
38aff96 TUI rewrite: streaming tokens, code mode, tool display, markdown
28f4c2a Consolidate into one pan binary with subcommands
a2ddaa3 Context assembler + fuzzing/load tests
e9820ba Wasm plugin lifecycle + async HTTP client
d7f6d4a Docs cleanup + GUI web frontend + TUI
774cbe3 Phase 4c: StreamingObservations for voice/streaming input
bef3c3a Phase 4b: multi-agent orchestration (cap.agent.delegate)
294843c Phase 4a: packaging docs, safety veto, gateway integration tests
6297313 Phase 3: streaming — token_tx in Loop, per-intent SSE gateway
06835ef Phase 2: config wiring + daemon ComponentRegistry unification
d26406e Phase 1 quick wins: CI, gateway tests, wasm plugin docs, daemon LLM converge
9b256d3 docs: update HANDOFF for Sprint 1-6 consolidation
6c1e6da Sprint 1-6 consolidation — gateway, async daemon, capabilities, providers, sandbox
eea59a2 pan-llm — TLS transport (rustls), so provider.llm reaches cloud endpoints
3601a8f pan-llm — tool-using LLM brain (provider.llm) plugged into the ReAct loop
9c3c949 agentic tool-use (ReAct) loop — a provider can use a tool, not just name one
fc818d3 docs: add HANDOFF.md for session continuity
ccc971e persistent cap.state (remembers across restarts)
c7cb11c interactive capabilities — cap.shell + provider.command
e71e100 pan-agent run — interactive REPL CLI
34ed905 close the arc — Agent.toml assembles a fully runnable agent
a8b5ebf executor/capability model — Toolbox + cap.state/cap.fs
ba3e43d Agent.toml manifest + assembler
33cc08e Python skill runtime (governed subprocess bridge)
ff17e72 async core + true cancellable abandon-path (D4)
eb2a127 Scope-aware governance, ScopedInvoker, ComponentRegistry (D1–D3)
```

The three RED issues from the original review are all resolved in working code.
The ADR's four decisions (D1–D4) are all landed. See the ADR's
"Implementation status" section for the full landed/pending list.

## Quick start (IMPORTANT env gotcha)

`cargo` is **not on PATH** in this environment — it's a rustup shim at
`~/.cargo/bin`. Prefix commands:

```sh
export PATH="$HOME/.cargo/bin:$PATH"

cargo test --workspace                              # 249 tests
cargo fmt --all --check                             # CI format gate
cargo clippy --workspace --all-targets -- -D warnings   # CI lint gate
( cd pan-core && bash verify.sh )                   # the compile-fail guards

# Run the interactive agent:
cargo build -p pan-daemon --bin pan
printf 'run echo hi\nremember pet cat\nrecall pet\n/quit\n' \
  | ./target/debug/pan run <Agent.toml>
```

A worked `Agent.toml` (command-driven, persistent memory):

```toml
[meta]
name = "doer"
persona = "assistant"
[persona]
provider = "provider.command"
[caps]
enable = ["cap.shell", "cap.state"]
[caps.grant]
shell = true
state = true
[caps.settings."cap.state"]
path = "memory.json"
```

## Crate map (10 crates)

| Crate | Role | Notes |
|---|---|---|
| `pan-core` | vocabulary, async pipeline/loop, Scope, ScopedInvoker, ComponentRegistry, Toolbox | the irreducible core; async via `async-trait`; type-state `Governed` invariant intact; ReAct loop + `TOOL_RESULT_CHANNEL`; `HostAllowlistGovernor` for `cap.http` URL policy; `Pipeline::execute_with_invoker` for cross-capability execution; `PipelineInvoker::sub()` for delegation; ContextBudget + ContextCompactor trait; GoalEvaluator trait; EffectHook + PathGovernor + PolicyChain |
| `pan-daemon` | Soul Protocol server (`pan serve`) | **fully async** (tokio TcpListener, tokio::spawn per perceive, AsyncBufReadExt/AsyncWriteExt framing); conformance 19/19; has its own single-shot local `llm.rs` |
| `pan-skill` | Python skill runtime + OS sandbox | `SkillRunner` spawns `python3`, services `cap.invoke` through a `ScopedInvoker`; `pan.py` embedded; `bwrap` sandbox (namespace isolation, cap-drop ALL, graceful fallback) |
| `pan-agent` | `Agent.toml` manifest + assembler | `assemble` → `AssembledAgent { scope, governor, provider, toolbox }`; `builtin_registry()`; providers `echo`/`command`/`rules`/`behaviortree`/`llm`/`anthropic`; context assemblers `rolling_history`/`memory_retrieval`/`session`; SessionStore |
| `pan-cap` | `cap.*` components | `cap.state` (KV, file-backed), `cap.fs` (rooted, path-jailed: read/write/list/glob/search/undo, auto-snapshot), `cap.shell` (direct exec), `cap.http` (GET/POST, blocking TCP), `cap.time` (ISO 8601 now/today), `cap.skill` (create/edit/list/delete/run lifecycle), `cap.format` (auto-format by extension), `cap.lsp` (diagnostics + format checks) |
| `pan-cli` | interactive REPL | `run_session` with cross-span conversation history (injects `history` channel fragment); the `pan` binary's `run` subcommand |
| `pan-llm` | tool-using LLM providers | `provider.llm`: OpenAI-compatible function calling on the ReAct loop; stateless transcript rebuild; retry/backoff on 429/5xx; std-only HTTP/1.0 over plain **or** rustls TLS (local + cloud BYOK); `provider.anthropic`: native Messages API; TruncationCompactor; LlmEvaluator |
| `pan-gateway` | HTTP gateway (`pan gateway` subcommand) | axum server: OpenAI-compatible `/v1/chat/completions`, Pan-native `/v1/agents/:name/goals`, agent delegation, streaming SSE, atomic metrics, Bearer-token auth; `AgentPool` loads from directory of `Agent.toml` files; serves TUI web frontend |
| `pan-tui` | Terminal UI (`pan tui` subcommand) | ratatui/crossterm: scrollable conversation, tool panel, streaming tokens, code mode (plan/build toggle, Tab), markdown rendering, input history, slash commands (/undo, /help, /clear, /quit) |

## The through-line (so the mental model transfers)

**`Agent.toml` → `assemble` → { Scope, ScopedGovernor, Provider, Toolbox } →
Pipeline + Loop → governed capability runs.**

- The **governor** decides *whether* a persona may reach a capability (by origin +
  capability-prefix grant); the **capability component** is *what runs*.
- The loop is **provider-agnostic**: echo/rules/BT/command (and a future LLM) all
  emit the same `ActionIntent`s. Never special-case a provider.
- Every effect goes through `resolve → validate → govern → execute`. There is no
  unscoped effect path; `EffectRequest` always carries a `Scope`.
- The loop is **agentic (ReAct)**: a decision that `Invoke`s without `Conclude`
  gets its results folded back into a per-goal working `Context` (fragments on
  `loop_engine::TOOL_RESULT_CHANNEL`) and the provider re-decides on the *same*
  goal — until it concludes, bounded by `MAX_TOOL_STEPS` (→ `RunEnd::StepLimit`).
  Providers that conclude in one step (all the current ones) never enter it. This
  is what lets a tool-using LLM see a result and act on it; the feedback is opaque
  `Context` a rules/BT provider ignores.

## Conventions this effort followed (keep them)

- **Every increment is committed only when fully green** (test + fmt + clippy +
  guards). Never leave the tree broken across a commit.
- **Commit style**: `type(scope): summary`, a body explaining the *why*, a final
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` line.
- **New crate ⇒ add**: workspace member (root `Cargo.toml`), a `README.md`, and a
  CI job in `.github/workflows/ci.yml` (mirror the `pan-cap`/`pan-cli` jobs).
- **After a meaningful change, update**: the ADR's Implementation-status section
  (move items landed→ / add to pending), and `CLAUDE.md`'s crate map if a crate
  or a load-bearing concept changed.
- CI lint job runs `fmt --all` + `clippy --workspace` at the repo root (covers all
  crates). Keep it that way.

## Non-obvious facts / gotchas

- **`pan-core/verify.sh`** links the rlib from the *workspace* target (resolved via
  `cargo metadata`), not `pan-core/target` (which is a stale standalone build).
  It treats rustc error-code drift as a WARNING (e.g. `handle_downcast` reports
  E0425 now, not the cited E0412) and only fails on a bypass that *compiles*.
- **Three binaries now**: `pan` (unified CLI: `pan serve`, `pan run`, `pan gateway`,
  `pan tui`), plus `pan-daemon`'s `check-conformance` subcommand. All four former
  entry points (`pan`, `pan-agent`, `pan-gateway`, `pan-tui`) consolidated into one
  binary. The cross-repo CI harness builds `--bin pan` (pan-daemon).
- **The daemon is now fully async**: tokio TcpListener, `tokio::spawn` per
  perceive, async read/write framing. The `block_on` bridge is gone from the
  server path; it remains only in the synchronous `on_perceive` fallback (used
  by tests). The daemon's `llm.rs` still uses a blocking client on the tokio
  thread — replacing it with a non-blocking one (or reusing `pan-llm`) is a
  future refinement.
- **`RunReport.results`** is an additive field: `(capability, return-value)` per
  executed effect, surfaced synchronously (don't read the off-thread event stream
  per-turn — it races). The CLI renders it.
- **`pan-skill` tests spawn real `python3`** (present here, `3.12`). They skip
  gracefully if it's absent.
- **Blocking-in-async, on purpose**: `cap.fs`, `cap.shell`, and the daemon's LLM
  client use blocking `std` I/O inside `async fn`, run on a dedicated thread /
  `block_on`. Documented as a future non-blocking refinement.
- **`cap.shell` runs programs directly** (no shell) — `args` is an explicit list,
  no metacharacter interpretation. Arg-level policy (a program allowlist) is a
  future *governor* concern, not a `cap.shell` one.

## Recipe: add a capability component (the common extension)

1. New struct in `pan-cap/src/<name>.rs` implementing
   `pan_core::toolbox::CapabilityProvider` (`id`, `capabilities`, async `execute`).
2. Export it in `pan-cap/src/lib.rs` and register a factory in
   `register_builtin_caps` (read its config from `cfg.settings`).
3. Unit tests in the module; if it composes with governance, an end-to-end test in
   `pan-cap/tests/end_to_end.rs`.
4. It's now selectable from any `Agent.toml` via `[caps.enable]` +
   `[caps.settings."cap.x"]`, and grantable via `[caps.grant]`.

Adding a **provider** is the same shape against `pan_core::schema::Provider`,
registered with `register_provider` in `pan-agent/src/builtin.rs`.

## What's next

**All ROADMAP items — including all 6 deferred phases — are landed. The feature
surface is complete.**

**Landed across all sprints, phases, and deferred items:**
- Sprints 1–10 (all items from the original ROADMAP) ✓
- Phase A: SnapshotStore + TUI /undo ✓
- Phase B: SessionStore — persistent JSONL conversation store ✓
- Phase C: ContextBudget + TruncationCompactor ✓
- Phase D: GoalEvaluator (LlmEvaluator) + RunEnd::Unsatisfied ✓
- Phase E: cap.lsp — language diagnostics + format checks ✓
- Phase F: Lifecycle hooks + PathGovernor + PolicyChain ✓

**Genuinely remaining polish:**
- **TUI real-time streaming** — tokens are currently buffered post-hoc. Real-time
  requires borrow-split refactor for `tokio::select!` racing key reads against
  token arrival.
- **Integration test breadth** — the new features lack dedicated tests (SessionStore
  round-trip, SnapshotStore/snapshot→undo, PolicyChain composition, TruncationCompactor,
  EffectHook, GoalEvaluator mock, LspCaps routing).
- **Documentation refresh** — ADR 0001's "Pending" section needs updating to reflect
  the six deferred items moving to landed.
- **Async daemon LLM** — the daemon's `llm.rs` still uses a blocking client on the
  tokio thread. Deferred: NPCs make single-shot `Express` calls, so blocking is
  acceptable. Convert when a streaming channel needs it.

Before starting any of these, confirm `git log` matches this doc's Status.
