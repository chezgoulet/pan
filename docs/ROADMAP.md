# Pan — What's Left to Build

_A comprehensive, honest map of everything not yet built, for the next session.
It complements [`HANDOFF.md`](HANDOFF.md) (current status, conventions, gotchas)
and [ADR 0001](decisions/0001-scope-invoker-components.md) (the binding
architecture). Read those two first; this document assumes them._

## Where things stand (one paragraph)

The **vertical slice is complete and load-bearing**: `Agent.toml` → an assembled,
origin-scoped, governed agent → a provider-agnostic loop that now supports
**agentic tool use (ReAct)** → capabilities that pass a non-bypassable
`resolve → validate → govern → execute` pipeline. There is a real LLM brain
(`provider.llm`) that uses tools over local HTTP **and** cloud TLS. 7 crates, all
green (tests + fmt + clippy + compile-fail guards + Soul Protocol conformance).

What's left is **breadth and depth on top of a settled core** — almost none of it
requires changing the core vocabulary or the pipeline invariants. The items below
are grouped by area, each with _what/why_, _where it plugs in_ (real files), an
_approach sketch_, a _testing strategy_, and _risks_. Nothing here is started.

## Recommended next three (if you want a path, not a menu)

1. **Context assembly + conversation memory** (§A) — the single biggest
   *functional* gap. Today the CLI is **amnesiac across turns** (`Context::default()`
   every line). This is what stops Pan from feeling like an assistant. Highest
   value, moderate effort, no new external deps.
2. **`cap.http`** (§C1) — a governed web-fetch tool. Small, fully testable against
   a localhost mock, and it makes the LLM agent genuinely useful (it can look
   things up). Natural pairing with the tool loop.
3. **LLM robustness** (§B2) — retries/backoff on 429/5xx, token-ish budgeting, and
   large-tool-output truncation. Cheap insurance that turns a demo into something
   you'd leave running.

Everything else is real but more specialized (daemon async, wasm, voice, skills
self-improvement). Pick by what the deployment in front of you needs.

---

## A. Context assembly & memory (the biggest functional gap)

**What & why.** `Context` (in `schema.rs`) is an ordered list of opaque
`Fragment { channel, body }`. The loop takes it as a *parameter* and never
assembles it — a deliberate Wave-0 punt (see `loop_engine.rs` docstring: "Context
assembly … is upstream of this"). Consequences today:

- **`pan-cli` passes `Context::default()` for every REPL line** (`pan-cli/src/lib.rs:75`).
  So a chat agent forgets the previous turn the instant it answers — there is no
  conversation history and no memory retrieval. The LLM reconstructs *within* a
  single span (tool exchanges), but nothing survives across spans.
- The `persona.instruction` is the only standing context; `cap.state` can persist
  facts but nothing *reads them back into the prompt*.

**Where it plugs in.**
- The seam already exists: `Loop::run_span(obs, ctx)` takes the `Context`. Build a
  **context assembler** that produces it per turn.
- `pan-core/src/handles.rs` already has the **read-only `MemoryQuery` handle**
  (the sibling to `ScopedInvoker`; a read grant that structurally cannot write —
  see the `handle_write.rs` compile-fail guard). This is the intended read path
  for memory.
- `AssembledAgent` (in `pan-agent/src/assembler.rs`) is where an assembler would
  be constructed from config and handed to the CLI/daemon.

**Approach sketch.**
1. Define a `ContextAssembler` trait (probably in `pan-core`): `async fn
   assemble(&self, goal: &Goal) -> Context`. Keep it a component (ComponentRegistry
   family) so `Agent.toml` selects it — mirrors providers/capabilities.
2. A first concrete assembler: **rolling conversation history**. Keep an in-memory
   (optionally `cap.state`-backed) transcript of prior `(user, assistant)` turns
   for the session; emit them as fragments on a `history` channel. `provider.llm`
   already folds non-`tool_result` fragments into the system prompt — but for
   history you'll likely want them as real prior `user`/`assistant` **messages**,
   so extend `OpenAiProvider::build_messages` to recognize a `history` channel and
   replay it as message turns (same pattern as the tool-exchange replay).
3. A second assembler: **memory retrieval** — query `cap.state` (or a future
   vector store) via a `MemoryQuery` handle and inject relevant facts on a `memory`
   channel.
4. Wire `run_session` to call the assembler each turn instead of `Context::default()`.

**Testing.** Unit-test the assembler (history accumulates, oldest trimmed at a cap);
extend the `pan-llm` mock test to assert the second *user* turn's request carries
the prior turn. No network.

**Risks.** Deciding history-as-fragments vs history-as-messages: the clean answer
is a dedicated channel the LLM provider interprets (keeps `Context` opaque to the
core). Don't let history leak into the core as a privileged concept — it is just
another channel a rules/BT provider ignores. Watch prompt growth (trim/summarize).

---

## B. LLM provider — polish & robustness (`pan-llm`)

The tool-use mapping and both transports are done. What's missing is production
hardening and reach.

### B1. Anthropic-native dialect (optional sibling provider)
- **Why.** `provider.llm` speaks OpenAI-compatible `/chat/completions`. Anthropic's
  *native* API (`/v1/messages`, `x-api-key` + `anthropic-version` headers, a
  different tool-use/`content` block shape) exposes features the compat endpoint
  doesn't. Only needed if you want those.
- **Where.** New module `pan-llm/src/anthropic.rs`; register `provider.anthropic`
  in `register_llm_providers` (`pan-llm/src/lib.rs`). Reuse `pan-llm::http` as-is
  (TLS already works) — just different headers, request body, and response parsing.
- **Approach.** Same three-part contract as `openai.rs`: caps → `tools`, a
  `tool_use` content block → `Invoke` (no `Conclude`), a text block → `Express` +
  `Conclude`. Reconstruct the transcript from `tool_result` fragments as
  `assistant`/`user`(tool_result) content blocks. The header auth is the only new
  transport wrinkle — `pan-llm::http::build_request` currently hardcodes
  `Authorization: Bearer`; generalize it to take extra headers.
- **Testing.** Mock-server unit tests mirroring `tests/tool_use.rs`, plus a
  credential-gated `live_cloud`-style test.

### B2. Robustness (do this before B1 — it protects every provider)
- **Retries/backoff** on HTTP 429 and 5xx (respect `Retry-After` when present).
  Today any non-200 is a one-shot `Conclude(Abandoned)` (`http.rs` → `parse_response`).
- **Timeouts/cancellation** are coarse (a 60s socket timeout + the loop's
  abandon-path). Fine for now; note it.
- **Token/turn budgeting.** `MAX_TOOL_STEPS` (in `loop_engine.rs`) caps tool
  *rounds* but there's no token accounting or cost ceiling. A budget belongs
  either in the provider (count usage from responses) or as a governor concern.
- **Large tool outputs.** A capability that returns a huge blob is replayed
  verbatim into the next prompt (`tool_result` fragment). Add truncation with a
  clear marker, ideally in the provider's `replay_exchange`.

### B3. Streaming responses
- **Why.** For a voice/interactive channel you want tokens as they arrive. The core
  already has the streaming/supersession machinery (the abandon-path); the missing
  piece is a provider that emits partial `Express`. This is bigger — it touches the
  `Provider::decide` contract (today it returns one `Decision`). Consider a separate
  streaming trait or an event-emitting side channel. **Defer** until a channel needs it.

---

## C. Capabilities (`pan-cap`)

The recipe is in `HANDOFF.md` ("add a capability component"). Each is a
`CapabilityProvider` in `pan-cap`, registered in `register_builtin_caps`, then
selectable from `Agent.toml`.

### C1. `cap.http` — governed web access (recommended)
- **What.** `cap.http.get` / `cap.http.post`, returning status + body. The thing
  that makes an LLM agent able to *look things up*.
- **Approach.** Reuse `pan-llm::http` patterns (or lift the client into a shared
  spot). Governance is the whole point: the grant is `http`, and arg-level policy
  (an allowlisted host set) is a **governor** concern, exactly like `cap.shell`'s
  program allowlist — don't bake policy into the capability.
- **Testing.** Localhost mock server (see `pan-llm/tests/tool_use.rs` for the
  pattern). No real network.
- **Risk.** SSRF / internal-network access — document that host-allowlisting is
  required for untrusted personas and lives in the governor.

### C2. Other capabilities worth having
- `cap.time` (clock/now — trivial, and models love to hallucinate dates).
- `cap.fs` **list/search/glob** (today it's read/write/list; richer traversal helps).
- `cap.state` **list/delete/namespaces** (today set/get only).
- `cap.process`/job control if a deployment needs long-running work.

---

## D. Skills — sandbox, lifecycle, self-improvement (`pan-skill`)

The governed subprocess bridge works; skills invoke capabilities through a
`ScopedInvoker` and cannot escalate. Two big gaps remain.

### D1. OS-level sandbox (the honest-scope gap)
- **What.** Today a skill's *unsanctioned Pan calls* are denied, but its *ambient*
  syscalls (open a file, a socket) are **not** — the Python subprocess runs with
  the daemon's privileges. The ADR is explicit about this being unfinished.
- **Where.** `SkillRunner::with_program` (`pan-skill/src/runner.rs`) is the seam:
  it already lets you swap the launcher.
- **Approach.** Wrap the `python3` invocation in `bwrap`/`nsjail` (or a
  namespaces + seccomp harness): no network, a read-only rootfs, a tmpfs work dir,
  drop caps. Make the sandbox profile configurable.
- **Testing.** A skill that tries to open a socket / write outside its jail must
  fail at the OS layer (gated on the sandbox binary being present, like the
  `python3` gate).
- **Risk.** Platform-specific; keep it opt-in and degrade clearly when the launcher
  is absent.

### D2. `skill.*` lifecycle capabilities + the self-improvement loop (Phase 7)
- **What.** `skill.create` / `skill.edit` / `skill.list` / `skill.delete` as
  *governed capabilities* wrapping `SkillRunner` — so an agent can author and run
  its own skills, under a scope that gates whether it may. This is the payoff of
  the whole scope/invoker design: a `meta.self-improve` origin with a narrow grant.
- **Approach.** A `SkillCaps` component (in a new crate or `pan-cap`) whose
  `execute` reads/writes skill files in a jailed dir and can launch them via the
  runner. Then a manifest that grants a persona `skill.*` closes the self-improvement
  loop: the agent proposes a skill, it's governed, it runs.
- **Risk.** This is the highest-authority surface in the system — treat the grant
  and the sandbox (D1) as prerequisites, not afterthoughts.

---

## E. Daemon — finish the async conversion & config-drive it (`pan-daemon`)

The daemon is functional and conformance-green, but architecturally behind the
rest of the workspace.

### E1. Fully async daemon (drop the `block_on` bridge)
- **What.** The daemon is thread-per-perceive and bridges to the async core via
  `pan_daemon::block_on` at two seams (`decide`, `dispatch_decision`). `llm.rs`
  uses a blocking client on the perceive thread.
- **Approach.** Convert `server.rs` (TCP loopback + NDJSON framing) and
  `session.rs` to tokio; give the daemon's `llm.rs` a non-blocking client (or reuse
  `pan-llm` — see E3). Only then does one slow soul stop occupying an OS thread.
- **Risk.** **Soul Protocol conformance (19 fixtures) must stay green** and the
  cross-repo harness must still pass. Do it behind the wire contract, incrementally.

### E2. Retire the daemon's hard-coded wiring onto `ComponentRegistry`
- **What.** The daemon builds providers/governor by hand; the rest of the workspace
  builds them from config via `ComponentRegistry`. Unify.
- **Risk.** The daemon's `ResolveGovernor<'a>` borrows the capability registry, so
  this is a real **lifetime restructuring** (build components into session-owned
  storage), not a mechanical swap. ADR calls this out as Phase-2, careful work.

### E3. Converge the two LLM implementations
- There are now **two** LLM clients: `pan-daemon/src/llm.rs` (single-shot Express,
  for game NPCs) and `pan-llm` (tool-using, both transports). Once the daemon is
  async, consider having it depend on `pan-llm` and delete its bespoke client — or
  deliberately keep the NPC one minimal. Decide, don't drift.

---

## F. Channels & deployment

`pan-cli` is the only channel. The loop is channel-agnostic (`Observations` in,
`Express`/`results` out), so channels are additive.

### F1. Streaming / voice channel
- **What.** A channel that yields evolving `Goal` **revisions** (partial ASR) and
  consumes partial `Express`. The core's abandon-path (`Observations::superseded`)
  was built for exactly this — a newer revision cancels the in-flight decide.
- **Where.** Implement a real `Observations` source (the CLI uses the degenerate
  `Once`). This is the "admission ↔ loop handoff for streaming" open question the
  `Observations` docstring names.
- **Depends on.** Streaming provider responses (§B3) for the output half.

### F2. Game / Soul Protocol integration (the daemon's reason to exist)
- The daemon already speaks the Soul Protocol; the host (Godot/REACHLOCK) supplies
  context and consumes decisions. Remaining work here is mostly E1/E2 plus whatever
  new message types the game side needs (keep fixtures byte-identical across repos).

### F3. Packaging & operability
- Binaries: `pan` (daemon) and `pan-agent` (CLI) exist. Missing: release profiles,
  a `--version`, install docs, example `Agent.toml`s under `examples/`.
- **Config unification.** `pan-core/src/config.rs` (`~/.pan/config.toml`, with
  imports + `${VAR}` + `PAN_` overrides) exists but is **not wired into the
  `Agent.toml` path**. Decide the relationship: global config vs per-agent manifest
  (e.g. global defaults an `Agent.toml` overrides).

---

## G. Wasm plugin system (`pan-core/src/plugind.rs`)

- **Status.** **Stubbed.** `WasmPlugin::load` and the provision/validate/run calls
  are `TODO(#62): instantiate wasmtime module and link the C-ABI exports`. The
  manifest parsing, `~/.pan/plugins/` discovery, and SIGHUP reload scaffolding
  exist; the actual wasmtime instantiation does not. Not exercised by the daemon.
- **What's left.** Add `wasmtime`, define the C-ABI (`plugin_provision` /
  `plugin_validate` / `plugin_run` exports + the host import table), implement
  instantiation and the invoke bridge, and enforce the manifest's declared
  capabilities at the boundary.
- **Note the deliberate distinction** (ADR 0001): **Component** = in-process trait
  impl selected by `Agent.toml` (done); **Plugin** = out-of-process/wasm
  (`plugind.rs`, this item). Don't conflate them. This is a large, self-contained
  effort — schedule it only when out-of-process/untrusted extension is actually
  needed.

---

## H. Cross-cutting concerns

### H1. Observability
- There is an off-thread ordered `EventStream` (`events.rs`) with pluggable sinks
  (`MemorySink`, `DiscardSink`). Missing: a real sink (structured `tracing`/JSON
  logs), and per-run metrics (tokens, tool calls, latency, denials). The stream is
  the natural home; `gov.audit` was noted in the ADR as "just an EventStream sink."

### H2. Hardware safety veto (§14, deferred)
- The abandon-path was built to be reused by a hardware safety veto (a decision in
  flight dropped before its effects reach the world). The plumbing exists; what's
  missing is *who sets the abandon signal* — a veto source feeding
  `Observations::superseded` (or an equivalent). Only relevant for robotics/game
  safety deployments.

### H3. Multi-agent / meta-agent orchestration
- `Scope` is hierarchical and `ScopedInvoker::sub()` narrows origins, so a
  meta-agent spawning sub-agents is expressible. Nothing *drives* it yet. If a
  deployment needs delegation, this is where it goes — and it's why the scope
  design exists.

### H4. Testing & conformance breadth
- Strong where it counts (compile-fail guards, conformance, ReAct e2e). Gaps: no
  property tests on the pipeline, no fuzzing of the wire/JSON, no load test of the
  daemon. Add as the surface hardens.

---

## Invariants to preserve (do not regress these while building the above)

- **The `Governed` type-state**: no ungoverned effect is expressible. The
  `compile-fail/` programs must keep failing to compile (`verify.sh`).
- **Origin-aware governance**: every `EffectRequest` carries a `Scope`; the core
  holds no policy. New capabilities/providers must not add an unscoped path.
- **Provider-agnosticism**: no provider is privileged. New context (history,
  memory, tool results) rides opaque `Context` channels a non-LLM provider ignores
  — never a core-level "chat" concept.
- **Soul Protocol conformance**: fixtures are byte-identical across repos; if one
  fails to deserialize, fix Pan, not the fixture.
- **Green-per-increment**: commit only when tests + fmt + clippy + guards +
  conformance all pass. Update this file, `HANDOFF.md`, and the ADR status as work
  lands.

---

## Quick index (file → where you'll work)

| Area | Primary files |
|---|---|
| Context/memory (§A) | `pan-core/src/handles.rs`, `pan-core/src/loop_engine.rs`, `pan-cli/src/lib.rs`, `pan-agent/src/assembler.rs`, `pan-llm/src/openai.rs` |
| LLM polish (§B) | `pan-llm/src/{openai,http,lib}.rs` |
| Capabilities (§C) | `pan-cap/src/*`, `pan-cap/src/lib.rs` |
| Skills (§D) | `pan-skill/src/runner.rs`, new `SkillCaps` component |
| Daemon async (§E) | `pan-daemon/src/{server,session,llm}.rs` |
| Channels (§F) | new channel crate/module; `pan-core::loop_engine::Observations` |
| Wasm plugins (§G) | `pan-core/src/plugind.rs` (TODO #62) |
| Observability/safety (§H) | `pan-core/src/events.rs`, `pan-core/src/schema.rs` (Scope) |
