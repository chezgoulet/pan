# Pan — Sprint Roadmap & What's Left to Build

_The sprint-generation guide for Pan. This document is the authoritative map of
what remains to build; it complements [`HANDOFF.md`](HANDOFF.md) (current status,
conventions, gotchas) and [ADR 0001](decisions/0001-scope-invoker-components.md)
(the binding architecture). Read both first; this document assumes them._

The doc has two views:

1. **§2 Sprint Plan** — the first-class view. A numbered sequence of sprints,
   each with an outcome, effort size, dependencies, items, and acceptance
   criteria. Generate sprints from here.
2. **§4 Reference Map** — the area-based reference (A–H) with _what/why/where/
   approach/risks_ for each item, preserved from the original map but annotated
   with effort and dependency metadata.

---

## 1. Current Baseline

**Branch:** `testing` at `774cbe3` (2026-07-22)
**Metrics:** 223 tests (all pass), 8 crates, 3 binaries, 4 compile-fail guards,
19 conformance tests covering 15 Soul Protocol fixtures (all green)
**Gate:** `cargo fmt --all --check` + `cargo clippy --workspace --all-targets -- -D warnings` clean
**Build:** `cargo build --workspace` (cargo is a rustup shim — `export PATH="$HOME/.cargo/bin:$PATH"`)

**What's built (everything below the line is landed):**

```
Agent.toml → assemble → a scoped, governed agent (pan-agent)
provider.llm + provider.anthropic (pan-llm) — tool-using LLM brains
ReAct loop (pan-core) — agentic tool-use with MAX_TOOL_STEPS bound
Capabilities: cap.state, cap.fs, cap.shell, cap.http, cap.time, cap.skill (pan-cap)
Providers: echo, command, rules, behavriortree, llm, anthropic
Python skill runtime + bwrap sandbox (pan-skill)
Soul Protocol daemon (pan-daemon) — fully async
Interactive CLI (pan-cli) — cross-span conversation history
HTTP gateway (pan-gateway) — OpenAI-compatible API, per-intent SSE
Global config merge (~/.pan/config.toml + Agent.toml)
ComponentRegistry unification (SessionPipeline, Arc<CapabilityRegistry>)
Hardware safety veto (VetoSource trait, third select! arm)
Multi-agent orchestration (cap.agent.delegate)
Voice/streaming input (StreamingObservations)
Observability: TracingSink, FnSink, property tests
Packaging: README, INSTALL, CHANGELOG, example agents
CI: .github/workflows/ci.yml
```

**What's NOT built:** The items below. All are additive — no core vocabulary or
pipeline changes needed.

**Caveat for the next session:** Check HANDOFF.md for the current commit log
before starting. The authoritative invariants are listed in §5.

---

## 2. Sprint Plan

### Landed: Sprint 2 — "Capability Fill-in"  [effort: S] ✓
### Landed: Sprint 3 — "Daemon Catches Up"  [effort: L] ✓
### Landed: Sprint 4 — "Daemon Unification"  [effort: M] ✓
### Landed: Sprint 5 — "Honest Sandbox + Self-Improvement"  [effort: L] ✓
### Landed: Sprint 6 — "More Providers, More Channels"  [effort: M] ✓

### Sprint 7 — "Wasm Plugins"  [effort: S]
### Sprint 8 — "Context Assembler"  [effort: M]
### Sprint 9 — "TUI (Terminal Agent)"  [effort: M]
### Sprint 10 — "GUI (Web Frontend)"  [effort: S]

### Deferred / Future

- **Async HTTP client** — both `pan-llm::http` and the daemon's LLM use blocking
  `TcpStream` inside `async fn`. A non-blocking HTTP client would let one slow
  inference call not occupy a tokio worker thread.
- **Memory retrieval assembler** — query `cap.state` via `MemoryQuery` handle and
  inject relevant facts on a `memory` channel. Builds on Sprint 8's assembler.
- **Testing breadth** — wire JSON fuzzing, daemon load test, stream cancellation
  fuzzing. Strong where it counts (compile-fail, conformance, ReAct).

---

## 3. Dependency Graph

```
All above the line is landed.

Sprint 7  (Wasm plugins)           — independent
Sprint 8  (Context Assembler)      — independent, additive
Sprint 9  (TUI)                    — depends on Sprint 6B streaming (landed)
Sprint 10 (GUI web frontend)       — depends on pan-gateway (landed)
```

All remaining items are additive with no core vocabulary or pipeline changes.

---

## 4. Reference Map (areas, with effort & dependency metadata)

The original area-based map, preserved, with each item annotated by effort size
and its sprint placement. Use this for detail beyond the sprint outlines above.

### A. Context assembly & memory (the biggest functional gap)  [Sprint 1A, Sprint 5 "memory retrieval" deferred]

**What & why.** `Context` (`schema.rs`) is an ordered list of opaque `Fragment { channel, body }`. The loop takes it as a *parameter* and never assembles it — a deliberate Wave-0 punt (see `loop_engine.rs` docstring: "Context assembly … is upstream of this"). Consequences today:

- **`pan-cli` passes `Context::default()` for every REPL line** (`pan-cli/src/lib.rs:75`). So a chat agent forgets the previous turn the instant it answers — no conversation history, no memory retrieval. The LLM reconstructs *within* a single span (tool exchanges), but nothing survives across spans.
- The `persona.instruction` is the only standing context; `cap.state` can persist facts but nothing *reads them back into the prompt*.

**Where it plugs in.** The seam already exists: `Loop::run_span(obs, ctx)` takes the `Context`. `pan-core/src/handles.rs` already has the read-only `MemoryQuery` handle (the sibling to `ScopedInvoker`; a read grant that structurally cannot write — see the `handle_write.rs` compile-fail guard). `AssembledAgent` (`pan-agent/src/assembler.rs`) is where an assembler would be constructed from config and handed to the CLI/daemon.

**Approach sketch.**
1. Define a `ContextAssembler` trait (probably in `pan-core`): `async fn assemble(&self, goal: &Goal) -> Context`. Keep it a component (ComponentRegistry family) so `Agent.toml` selects it — mirrors providers/capabilities.
2. A first concrete assembler: **rolling conversation history**. Keep an in-memory (optionally `cap.state`-backed) transcript of prior `(user, assistant)` turns for the session; emit them as fragments on a `history` channel. `provider.llm` already folds non-`tool_result` fragments into the system prompt — but for history you'll likely want them as real prior `user`/`assistant` **messages**, so extend `OpenAiProvider::build_messages` to recognize a `history` channel and replay it as message turns (same pattern as the tool-exchange replay).
3. A second assembler: **memory retrieval** — query `cap.state` (or a future vector store) via a `MemoryQuery` handle and inject relevant facts on a `memory` channel. (Deferred — see Deferred/Future above.)
4. Wire `run_session` to call the assembler each turn instead of `Context::default()`.

**Testing.** Unit-test the assembler (history accumulates, oldest trimmed at a cap); extend the `pan-llm` mock test to assert the second *user* turn's request carries the prior turn. No network.

**Risks.** Deciding history-as-fragments vs history-as-messages: the clean answer is a dedicated channel the LLM provider interprets (keeps `Context` opaque to the core). Don't let history leak into the core as a privileged concept — it is just another channel a rules/BT provider ignores. Watch prompt growth (trim/summarize).

---

### B. LLM provider — polish & robustness (`pan-llm`)  [Sprint 1C, Sprint 6]

The tool-use mapping and both transports are done. What's missing is production hardening and reach.

#### B1. Anthropic-native dialect (optional sibling provider)  [Sprint 6A]
- **Why.** `provider.llm` speaks OpenAI-compatible `/chat/completions`. Anthropic's *native* API (`/v1/messages`, `x-api-key` + `anthropic-version` headers, a different tool-use/`content` block shape) exposes features the compat endpoint doesn't. Only needed if you want those.
- **Where.** New module `pan-llm/src/anthropic.rs`; register `provider.anthropic` in `register_llm_providers` (`pan-llm/src/lib.rs`). Reuse `pan-llm::http` as-is (TLS already works) — just different headers, request body, and response parsing.
- **Approach.** Same three-part contract as `openai.rs`: caps → `tools`, a `tool_use` content block → `Invoke` (no `Conclude`), a text block → `Express` + `Conclude`. Reconstruct the transcript from `tool_result` fragments as `assistant`/`user`(tool_result) content blocks. The header auth is the only new transport wrinkle — `pan-llm::http::build_request` currently hardcodes `Authorization: Bearer`; generalize it to take extra headers.
- **Testing.** Mock-server unit tests mirroring `tests/tool_use.rs`, plus a credential-gated `live_cloud`-style test.

#### B2. Robustness (do this before B1 — it protects every provider)  [Sprint 1C]
- **Retries/backoff** on HTTP 429 and 5xx (respect `Retry-After` when present). Today any non-200 is a one-shot `Conclude(Abandoned)` (`http.rs` → `parse_response`).
- **Timeouts/cancellation** are coarse (a 60s socket timeout + the loop's abandon-path). Fine for now; note it.
- **Token/turn budgeting.** `MAX_TOOL_STEPS` (in `loop_engine.rs`) caps tool *rounds* but there's no token accounting or cost ceiling. A budget belongs either in the provider (count usage from responses) or as a governor concern.
- **Large tool outputs.** A capability that returns a huge blob is replayed verbatim into the next prompt (`tool_result` fragment). Add truncation with a clear marker, ideally in the provider's `replay_exchange`.

#### B3. Streaming responses  [Sprint 6B, deferred]
- **Why.** For a voice/interactive channel you want tokens as they arrive. The core already has the streaming/supersession machinery (the abandon-path); the missing piece is a provider that emits partial `Express`. This is bigger — it touches the `Provider::decide` contract (today it returns one `Decision`). Consider a separate streaming trait or an event-emitting side channel. **Defer** until a channel needs it.

---

### C. Capabilities (`pan-cap`)

The recipe is in `HANDOFF.md` ("add a capability component"). Each is a `CapabilityProvider` in `pan-cap`, registered in `register_builtin_caps`, then selectable from `Agent.toml`.

#### C1. `cap.http` — governed web access (recommended)  [Sprint 1B]
- **What.** `cap.http.get` / `cap.http.post`, returning status + body. The thing that makes an LLM agent able to *look things up*.
- **Approach.** Reuse `pan-llm::http` patterns (or lift the client into a shared spot). Governance is the whole point: the grant is `http`, and arg-level policy (an allowlisted host set) is a **governor** concern, exactly like `cap.shell`'s program allowlist — don't bake policy into the capability.
- **Testing.** Localhost mock server (see `pan-llm/tests/tool_use.rs` for the pattern). No real network.
- **Risk.** SSRF / internal-network access — document that host-allowlisting is required for untrusted personas and lives in the governor.

#### C2. Other capabilities worth having  [Sprint 2]
- `cap.time` (clock/now — trivial, and models love to hallucinate dates).
- `cap.fs` **list/search/glob** (today it's read/write/list; richer traversal helps).
- `cap.state` **list/delete/namespaces** (today set/get only).
- `cap.process`/job control if a deployment needs long-running work.

---

### D. Skills — sandbox, lifecycle, self-improvement (`pan-skill`)  [Sprint 5]

The governed subprocess bridge works; skills invoke capabilities through a `ScopedInvoker` and cannot escalate. Two big gaps remain.

#### D1. OS-level sandbox (the honest-scope gap)  [Sprint 5A]
- **What.** Today a skill's *unsanctioned Pan calls* are denied, but its *ambient* syscalls (open a file, a socket) are **not** — the Python subprocess runs with the daemon's privileges. The ADR is explicit about this being unfinished.
- **Where.** `SkillRunner::with_program` (`pan-skill/src/runner.rs`) is the seam: it already lets you swap the launcher.
- **Approach.** Wrap the `python3` invocation in `bwrap`/`nsjail` (or a namespaces + seccomp harness): no network, a read-only rootfs, a tmpfs work dir, drop caps. Make the sandbox profile configurable.
- **Testing.** A skill that tries to open a socket / write outside its jail must fail at the OS layer (gated on the sandbox binary being present, like the `python3` gate).
- **Risk.** Platform-specific; keep it opt-in and degrade clearly when the launcher is absent.

#### D2. `skill.*` lifecycle capabilities + the self-improvement loop (Phase 7)  [Sprint 5B]
- **What.** `skill.create` / `skill.edit` / `skill.list` / `skill.delete` as *governed capabilities* wrapping `SkillRunner` — so an agent can author and run its own skills, under a scope that gates whether it may. This is the payoff of the whole scope/invoker design: a `meta.self-improve` origin with a narrow grant.
- **Approach.** A `SkillCaps` component (in a new crate or `pan-cap`) whose `execute` reads/writes skill files in a jailed dir and can launch them via the runner. Then a manifest that grants a persona `skill.*` closes the self-improvement loop: the agent proposes a skill, it's governed, it runs.
- **Risk.** This is the highest-authority surface in the system — treat the grant and the sandbox (D1) as prerequisites, not afterthoughts.

---

### E. Daemon — finish the async conversion & config-drive it (`pan-daemon`)  [Sprint 3, Sprint 4]

The daemon is functional and conformance-green, but architecturally behind the rest of the workspace.

#### E1. Fully async daemon (drop the `block_on` bridge)  [Sprint 3]
- **What.** The daemon is thread-per-perceive and bridges to the async core via `pan_daemon::block_on` at two seams (`decide`, `dispatch_decision`). `llm.rs` uses a blocking client on the perceive thread.
- **Approach.** Convert `server.rs` (TCP loopback + NDJSON framing) and `session.rs` to tokio; give the daemon's `llm.rs` a non-blocking client (or reuse `pan-llm` — see E3). Only then does one slow soul stop occupying an OS thread.
- **Risk.** **Soul Protocol conformance (19 conformance tests, 15 fixtures) must stay green** and the cross-repo harness must still pass. Do it behind the wire contract, incrementally.

#### E2. Retire the daemon's hard-coded wiring onto `ComponentRegistry`  [Sprint 4]
- **What.** The daemon builds providers/governor by hand; the rest of the workspace builds them from config via `ComponentRegistry`. Unify.
- **Risk.** The daemon's `ResolveGovernor<'a>` borrows the capability registry, so this is a real **lifetime restructuring** (build components into session-owned storage), not a mechanical swap. ADR calls this out as Phase-2, careful work.

#### E3. Converge the two LLM implementations  [Sprint 3/4]
- There are now **two** LLM clients: `pan-daemon/src/llm.rs` (single-shot Express, for game NPCs) and `pan-llm` (tool-using, both transports). Once the daemon is async (Sprint 3), consider having it depend on `pan-llm` and delete its bespoke client — or deliberately keep the NPC one minimal. Decide, don't drift.

---

### F. Channels & deployment

`pan-cli` is the only channel. The loop is channel-agnostic (`Observations` in, `Express`/`results` out), so channels are additive.

#### F1. Streaming / voice channel  [Deferred — depends on Sprint 6B]
- **What.** A channel that yields evolving `Goal` **revisions** (partial ASR) and consumes partial `Express`. The core's abandon-path (`Observations::superseded`) was built for exactly this — a newer revision cancels the in-flight decide.
- **Where.** Implement a real `Observations` source (the CLI uses the degenerate `Once`). This is the "admission ↔ loop handoff for streaming" open question the `Observations` docstring names.
- **Depends on.** Streaming provider responses (§B3 / Sprint 6B) for the output half.

#### F2. Game / Soul Protocol integration (the daemon's reason to exist)  [Sprint 3/4 + as needed]
- The daemon already speaks the Soul Protocol; the host (Godot/REACHLOCK) supplies context and consumes decisions. Remaining work here is mostly E1/E2 plus whatever new message types the game side needs (keep fixtures byte-identical across repos).

#### F3. Packaging & operability  [Deferred]
- Binaries: `pan` (daemon) and `pan-agent` (CLI) exist. Missing: release profiles, a `--version`, install docs, example `Agent.toml`s under `examples/`.
- **Config unification.** `pan-core/src/config.rs` (`~/.pan/config.toml`, with imports + `${VAR}` + `PAN_` overrides) exists but is **not wired into the `Agent.toml` path**. Decide the relationship: global config vs per-agent manifest (e.g. global defaults an `Agent.toml` overrides).

---

### G. Wasm plugin system (`pan-core/src/plugind.rs`)  [Sprint 7A]

- **Status.** **Stubbed.** `WasmPlugin::load` and the provision/validate/run calls are `TODO(#62): instantiate wasmtime module and link the C-ABI exports`. The manifest parsing, `~/.pan/plugins/` discovery, and SIGHUP reload scaffolding exist; the actual wasmtime instantiation does not. Not exercised by the daemon.
- **What's left.** Add `wasmtime`, define the C-ABI (`plugin_provision` / `plugin_validate` / `plugin_run` exports + the host import table), implement instantiation and the invoke bridge, and enforce the manifest's declared capabilities at the boundary.
- **Note the deliberate distinction** (ADR 0001): **Component** = in-process trait impl selected by `Agent.toml` (done); **Plugin** = out-of-process/wasm (`plugind.rs`, this item). Don't conflate them. This is a large, self-contained effort — schedule it only when out-of-process/untrusted extension is actually needed.

---

### H. Cross-cutting concerns

#### H1. Observability  [Sprint 7B]
- There is an off-thread ordered `EventStream` (`events.rs`) with pluggable sinks (`MemorySink`, `DiscardSink`). Missing: a real sink (structured `tracing`/JSON logs), and per-run metrics (tokens, tool calls, latency, denials). The stream is the natural home; `gov.audit` was noted in the ADR as "just an EventStream sink."

#### H2. Hardware safety veto (§14, deferred)
- The abandon-path was built to be reused by a hardware safety veto (a decision in flight dropped before its effects reach the world). The plumbing exists; what's missing is *who sets the abandon signal* — a veto source feeding `Observations::superseded` (or an equivalent). Only relevant for robotics/game safety deployments.

#### H3. Multi-agent / meta-agent orchestration
- `Scope` is hierarchical and `ScopedInvoker::sub()` narrows origins, so a meta-agent spawning sub-agents is expressible. Nothing *drives* it yet. If a deployment needs delegation, this is where it goes — and it's why the scope design exists.

#### H4. Testing & conformance breadth
- Strong where it counts (compile-fail guards, conformance, ReAct e2e). Gaps: no property tests on the pipeline, no fuzzing of the wire/JSON, no load test of the daemon. Add as the surface hardens.

---

## 5. Invariants to preserve (do not regress these while building the above)

- **The `Governed` type-state**: no ungoverned effect is expressible. The `compile-fail/` programs must keep failing to compile (`verify.sh`).
- **Origin-aware governance**: every `EffectRequest` carries a `Scope`; the core holds no policy. New capabilities/providers must not add an unscoped path.
- **Provider-agnosticism**: no provider is privileged. New context (history, memory, tool results) rides opaque `Context` channels a non-LLM provider ignores — never a core-level "chat" concept.
- **Soul Protocol conformance**: fixtures are byte-identical across repos; if one fails to deserialize, fix Pan, not the fixture.
- **Green-per-increment**: commit only when tests + fmt + clippy + guards + conformance all pass. Update this file, `HANDOFF.md`, and the ADR status as work lands.

---

## 6. Quick index (file → where you'll work)

| Area | Primary files |
|---|---|
| Context/memory (§A, Sprint 1A) | `pan-core/src/handles.rs`, `pan-core/src/loop_engine.rs`, `pan-cli/src/lib.rs`, `pan-agent/src/assembler.rs`, `pan-llm/src/openai.rs` |
| LLM polish (§B, Sprints 1C/6) | `pan-llm/src/{openai,http,lib}.rs` |
| Capabilities (§C, Sprints 1B/2) | `pan-cap/src/*`, `pan-cap/src/lib.rs` |
| Skills (§D, Sprint 5) | `pan-skill/src/runner.rs`, new `SkillCaps` component |
| Daemon async (§E, Sprints 3/4) | `pan-daemon/src/{server,session,llm}.rs` |
| Channels (§F) | new channel crate/module; `pan-core::loop_engine::Observations` |
| Wasm plugins (§G, Sprint 7A) | `pan-core/src/plugind.rs` (TODO #62) |
| Observability/safety (§H, Sprint 7B) | `pan-core/src/events.rs`, `pan-core/src/schema.rs` (Scope) |

---

## 7. Revision Log

- **v3** (2026-07-22): Baseline updated to `774cbe3` (all Sprints 1–6 landed).
  Phases 1–4 added: streaming SSE, config merge, daemon unification, safety veto,
  multi-agent, voice/streaming observations, gateway integration tests, packaging,
  CI. New sprints: Context Assembler (S8), TUI (S9), GUI (S10).
- **v2** (2026-07-19): Restructured for sprint planning. Added the **Sprint Plan** (§2)
  as the first-class view, a **Dependency Graph** (§3), effort-size metadata throughout,
  and this revision log.
- **v1** (2026-07-19): Original area-based reference map (`ROADMAP.md`).
