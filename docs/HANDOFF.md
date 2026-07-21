# Handoff — Pan extended buildout (ADR 0001)

_Living continuity doc. Update the "Status" and "What's next" sections as work
lands. The authoritative design is [ADR 0001](decisions/0001-scope-invoker-components.md);
the always-loaded orientation is [`/CLAUDE.md`](../CLAUDE.md). Read both first._

## Status (branch `testing`, pushed to `origin/testing`)

Pan is a **runnable, governed, interactive, tool-using agent assembled from
`Agent.toml`** — with a real LLM brain (`provider.llm`) that *uses* tools — plus
a Python skill runtime and the Soul Protocol daemon. Everything below is
**green**: 159 tests, workspace `fmt` + `clippy -D warnings` clean, the four
`pan-core` compile-fail guards hold, and Soul Protocol conformance is 19/19.

This effort added these commits on top of `f16fd15` (each a coherent, green step):

```
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

cargo test --workspace                              # 159 tests
cargo fmt --all --check                             # CI format gate
cargo clippy --workspace --all-targets -- -D warnings   # CI lint gate
( cd pan-core && bash verify.sh )                   # the compile-fail guards

# Run the interactive agent:
cargo build -p pan-cli --bin pan-agent
printf 'run echo hi\nremember pet cat\nrecall pet\n/quit\n' \
  | ./target/debug/pan-agent run <Agent.toml>
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

## Crate map (7 crates)

| Crate | Role | Notes |
|---|---|---|
| `pan-core` | vocabulary, async pipeline/loop, Scope, ScopedInvoker, ComponentRegistry, Toolbox | the irreducible core; async via `async-trait`; type-state `Governed` invariant intact; ReAct loop + `TOOL_RESULT_CHANNEL` |
| `pan-daemon` | Soul Protocol server (`pan serve`) | thread-per-perceive; bridges to async core via `pan_daemon::block_on`; conformance 19/19; has its own single-shot local `llm.rs` |
| `pan-skill` | Python skill runtime | `SkillRunner` spawns `python3`, services `cap.invoke` through a `ScopedInvoker`; `pan.py` embedded |
| `pan-agent` | `Agent.toml` manifest + assembler | `assemble` → `AssembledAgent { scope, governor, provider, toolbox }`; `builtin_registry()`; providers `echo`/`command`/`rules`/`behaviortree`/`llm` |
| `pan-cap` | `cap.*` components | `cap.state` (KV, optionally file-backed), `cap.fs` (rooted, path-jailed), `cap.shell` (direct exec) |
| `pan-cli` | interactive REPL | `run_session`; the `pan-agent` binary (distinct from daemon's `pan`) |
| `pan-llm` | tool-using LLM providers | `provider.llm`: OpenAI-compatible function calling mapped onto the ReAct loop; stateless transcript rebuild; std-only HTTP/1.0 over plain **or** rustls TLS (local + cloud BYOK) |

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
- **Two binaries named differently**: `pan` (pan-daemon, `pan serve`) and
  `pan-agent` (pan-cli, `pan-agent run`). Don't make a second `pan` — output paths
  would collide. The cross-repo CI harness builds pan-daemon's `--bin pan`.
- **The daemon is not fully async**: it bridges to the async core with
  `pan_daemon::block_on` at two seams (`decide`, `dispatch_decision`). Dropping
  that bridge (fully-async server/session, non-blocking LLM client) is pending.
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

## What's next (all incremental — the load-bearing architecture is done)

**The authoritative sprint plan lives in [`ROADMAP.md`](ROADMAP.md#2-sprint-plan)** —
it is the sprint-generation guide: a numbered sequence with outcomes, effort
sizes, dependencies, per-item detail, and acceptance criteria. The short list
below is the recommended near-term order; read the ROADMAP for the rest.

**Sprint 1 (recommended first):**
1. **Context assembly + conversation memory** (`ROADMAP §A, Sprint 1A`) — the
   single biggest *functional* gap. The CLI passes `Context::default()` every
   line; fix this first so the agent remembers the prior turn. Highest value,
   moderate effort, no new external deps.
2. **`cap.http`** (`ROADMAP §C1, Sprint 1B`) — governed web access. Makes the LLM
   agent genuinely useful (it can look things up). Test against a localhost mock.
3. **LLM robustness** (`ROADMAP §B2, Sprint 1C`) — retries/backoff on 429/5xx,
   large-tool-output truncation. Cheap insurance that turns a demo into something
   you'd leave running.

The remaining sprints (capability fill-in, daemon async + unification, skill
sandbox + self-improvement, Anthropic provider + streaming, wasm + observability)
are all in the ROADMAP with their own dependency chains and acceptance criteria.

Before starting any of these, re-read the ADR and confirm the current `git log`
matches this doc's Status (update the Status if it has moved).
