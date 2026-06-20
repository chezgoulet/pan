# Pan — Complete Specification, Roadmap & Plan

**Version:** 1.0 (compiled)
**Status:** Design settled; ready to build. Supersedes the working drafts.
**Nature:** A minimal, plugin-based agent harness in Rust. The core is the irreducible loop
that makes an agent act; everything else is a plugin. The same core, with different plugin
sets, drives a chat assistant, game-NPC brains, and headless trend detection.

**Why it exists:** to build a tool worth depending on, built well. Adoption is not a goal.
Every decision below is justified by correctness and long-term maintainability, not
popularity. That single freedom is what lets the architecture be uncompromising — there is
no pressure to lower a barrier or court contributors.

---

## Table of contents

1. Executive summary
2. Lessons learned (what shaped the design)
3. The settled design — core
4. The settled design — vocabulary (`Goal` / `ActionIntent`)
5. The dispatch pipeline
6. The plugin model
7. The six resolved design questions
8. The plugin taxonomy (families)
9. The layered design model (rings)
10. Hardware safety boundary (deferred, contract preserved)
11. Performance expectations
12. Build roadmap (waves)
13. The reference deployment: Hermes/OpenClaw replacement
14. Known seams & deferred problems
15. Appendix: the validated schema

---

## 1. Executive summary

Pan is built on one decision: find the *smallest* core that can drive an agent well, and
push everything else — even things that feel essential, like the reasoning model, persistent
identity, and chat — outside it as plugins. The test applied throughout was concrete: a
responsibility belongs in the core only if **every** target deployment needs it (chat
assistant, NPC, trend detector). By that test the core is three things:

1. **A non-bypassable typed dispatch pipeline** — where every side-effecting action passes
   `resolve → validate → govern → execute → record`, and the unsafe path cannot be built.
2. **A deliberately boring four-phase loop** — `observe → decide → enact → commit`,
   stream-driven, wrapping the pipeline and the provider.
3. **An event stream** — the ordered, typed record everything else hangs off.

Everything else — the LLM, the soul/identity, memory, context assembly, channels, governance
policy, scheduling, admission — is a plugin. The reasoning backend being a plugin is the
keystone: it means the core contains no prompt, no token format, and no tool-call convention,
which is what lets a behavior tree or a rules engine stand in for an LLM without pretending to
be one.

The headline consequence: reconstructing a Hermes/OpenClaw-style assistant is **a plugin
manifest plus ~5 assistant-specific plugins**, not a special build. If that holds true at
delivery, the core/plugin boundary was drawn correctly.

---

## 2. Lessons learned (what shaped the design)

Studied as positive and negative references:

- **Hermes (Python, loop-centric, self-improving).** *Right:* provider abstraction; markdown
  skills as code-free extension; explicit toolset *distributions* (different tool bundles per
  deployment — the "same core, different plugins" thesis already shipped); six execution
  backends (proving the sandbox is a plugin slot). *Wrong:* a 200-line kitchen-sink config; the
  loop fused to TUI, gateway, cron, and platform adapters; opaque memory.

- **OpenClaw (TypeScript, gateway-centric).** *Right:* a clear control-plane/extension split;
  ~345k stars proving people want a hackable, self-hostable, on-prem assistant — hackability
  *is* the product. *Wrong:* credential isolation rated weak; tools run on the host with full
  access; a session object that accreted docking, pruning, compaction, steering, presence until
  the "core" was no longer portable; heartbeats that wake the whole agent on a fixed timer.

- **OpenAI Agents SDK.** *Right:* one `Run()` entry point; a ~20-line deterministic turn loop;
  no opinion on memory. *Wrong:* guardrails bundled into the SDK; an Agent object conflating
  identity, config, and tool registry; tied to one model API.

- **Anthropic patterns.** *Right:* system prompt first-class; native tool-use as the cleanest
  primitive. *Wrong:* no framework — you rewrite the loop each time.

- **Vercel AI SDK.** *Right:* provider-agnostic core; simplest-possible multi-step abstraction;
  per-tool error handling. *Wrong:* JS/TS only; experimental, complex middleware.

- **Caddy (plugin-architecture reference).** *Right:* self-registration by hierarchical ID; a
  minimal module interface with capability via optional interfaces; a clean lifecycle
  (Register → Provision → Validate → Run → Cleanup). *Wrong:* compile-time-only registration;
  fragile last-registration-wins conflict resolution.

**The dominant lesson — core creep via the session object.** In OpenClaw the session became a
god-object. The mitigation is structural: the core holds no shared mutable god-state; every
behavior that reads or writes run state is a plugin operating through a narrow handle.

**The strongest cross-cutting insight:** both reference agents put nearly everything outside
the core and only disagreed on *where the membrane sits*. For a harness that must also drive
non-chat deployments, the membrane goes around the **loop**, not the chat plumbing.

---

## 3. The settled design — core

The core owns exactly three things (§1). What is **explicitly not** in the core: the reasoning
model, prompts, token formats, tool-call parsing, durable memory, identity/soul, channels,
cron, UI, sandbox choice, and admission policy. Each is a plugin or a slot-filler.

### 3.1 The litmus test

A responsibility is core only if the chat assistant, the NPC, and the trend detector **all**
need it. This test is falsifiable and was applied to every candidate; it is what demoted soul
and admission (below) out of the core.

### 3.2 The internal vocabulary

Because the provider is a plugin, the loop speaks only an internal, provider-agnostic language:

> goal + assembled context + available capabilities → zero or more action-intents

An action-intent is **not** a tool call. A `provider.llm` plugin translates LLM tool-use into
action-intents; a `provider.behaviortree` emits the same intents from a tree tick; a
`provider.rules` from rule evaluation. The core cannot tell which produced them. This is the
keystone that makes non-LLM deployments real rather than aspirational.

### 3.3 The loop

`observe → decide → enact → commit`, stream-driven:

- **observe** — accumulate what this span may see (plugin concern: a prompt, a sensor window,
  a blackboard). Admission/segmentation lives here as a plugin.
- **decide** — hand a coherent snapshot to the provider; receive intents.
- **enact** — run side-effecting intents through the dispatch pipeline.
- **commit** — persist changed state (via a state handle) and finalize the event record.

The loop is a long-lived process consuming an **observation stream**; "a run" is a *span*
within it. The familiar discrete request/response is the degenerate case: a single-observation
span. (See §7, Resolution 1.)

---

## 4. The settled design — vocabulary

The make-or-break contract: the typed shape an LLM provider **and** a non-LLM provider can
both emit *natively*. Validated by implementing three providers against it in one file (§15).

### 4.1 `Goal`

Carries `id` (the span identity) and `revision` (a monotonic token). A new revision of an
in-flight goal **supersedes** the prior; the loop always decides on the latest, and a
`Decision` whose goal was superseded is discarded at the `enact` boundary rather than executed.
The trigger that produced the goal is normalized (`Utterance` / `Tick` / `Event` / `Signal`)
so a chat message, a cron tick, a game event, and a sensor threshold all enter identically.

### 4.2 `ActionIntent` — three variants

The crux. A tool call is **one variant, not the whole type** — this is what de-privileges the
LLM.

- **`Invoke { capability, args, correlation? }`** — *all* world-effects, including state
  writes. `correlation` is optional: LLMs set it to match results; behavior trees and rules
  decline it (declining, not fabricating — if it were required, non-LLM providers would have to
  invent fake IDs, and the schema would have leaked).
- **`Express { body }`** — emit content to whoever is listening. NOT inherently chat: a line of
  NPC dialogue, a chat reply, an alert body. Control-only providers simply never emit it.
- **`Conclude { outcome }`** — signal the span resolved/abandoned/continues. Replaces the LLM's
  `stop_reason` with something all three providers can produce.

**Why only three (the resolved `Mutate` question):** state-writes are `Invoke` of a capability
with a state-write permission class — *not* a separate variant. Every argument for a separate
`Mutate` was actually an argument about governance, which is the `govern` stage's job. Unifying
gives the pipeline exactly one effect-path to gate, eliminating a class of "someone added a
second path that skips a check" drift. `Express` and `Conclude` stay separate because they are
genuinely *not* world-effects — folding them into `Invoke` would be the opposite error.

---

## 5. The dispatch pipeline

**The heart of Pan.** Every side-effecting action-intent passes through one fixed sequence; the
**sequence is core**, the **stage implementations are plugins**.

```
resolve  →  validate  →  govern  →  execute  →  record
(name to    (args vs     (policy,   (in-proc    (event
 capability  schema)      secrets,   or RPC)      stream)
 binding)                 rate,
                          audit,
                          approval)
```

- No plugin can register "a hook that executes a capability." Execution happens *only* at
  `execute`, reachable *only* after `govern` returns allow. This structurally removes the
  dispatch/governance entanglement.
- A governance plugin only ever receives the `govern` call with a typed decision interface
  (`Allow` / `Deny` / `RequireApproval`); it is never handed the executor, so it physically
  cannot perform execution.
- `resolve` decides in-process vs RPC/MCP transport invisibly to the loop — the capability's
  registration declares its transport. One interface, two transports.
- Observation hooks (`BeforeDecide`, `AfterDecide`, `OnEvent`) exist but are **read-only**:
  logging, metrics, tracing. They cannot mutate intents or bypass stages.

In Rust this is enforced by types: a capability that has not passed `govern` cannot be
constructed in the form `execute` accepts. The unsafe path does not compile.

---

## 6. The plugin model

### 6.1 One interface, two transports

Every capability registers with: a **name** (hierarchical ID), **input/output schemas**
(validated by the core at dispatch), a **permission class**, and a **resolution**
(`in-process` function or `rpc` remote). The loop knows only the interface; the transport is
invisible. In-process for fast, trusted primitives; RPC for heavy or untrusted work.

### 6.2 Capability handles (no shared god-state)

Plugins never receive other plugins. They receive **trait-object handles granted at provision
time**, exposing exactly the allowed operations. A context plugin needing memory receives an
`Arc<dyn MemoryQuery>` (read-only, `Retrieve` only); there is no write method on the trait and
no downcast path to the concrete memory plugin. **The trait's surface *is* the enforcement** —
absence of a write method is absence of write capability, checked at compile time.

The precise sync-vs-async rule: **synchronous cross-family reads** via granted handles
(context assembling a turn may read memory *now*); **asynchronous reactions/writes** via the
event stream (memory updating *because* something happened). No plugin holds a write handle to
a resource another family owns.

### 6.3 Lifecycle (Caddy-derived)

`Register` (by hierarchical ID) → `Provision` (deps, config, handle injection) → `Validate` →
`Run` → `Cleanup`. Conflict resolution is explicit: two plugins claiming the same slot+ID is a
**provision-time error**, never a silent last-wins override.

---

## 7. The six resolved design questions

These were the open questions; all are now settled.

**1. Discrete loop vs. stream — RESOLVED: stream.** The loop is a continuous process consuming
an observation stream; "a run" is a span. The discrete model is a degenerate single-observation
span — the special case, not the unit. Crucially, the **provider still sees a discrete
`decide()` call** with a coherent snapshot; streaming lives in how observations accumulate and
when a span concludes, *not* in the provider contract. So streaming did not force a provider
redesign — it shaped the loop and `Goal`.

**2. `Goal` fixed-at-start — RESOLVED: no, it carries a revision.** A superseded in-flight goal
is abandoned at `enact`. This shares one mechanism with the §10 safety veto: "cleanly abandon an
in-flight decision" is built once and used for both streaming-supersession and hardware-veto.

**3. `Mutate` separate vs. unified — RESOLVED: unified into `Invoke`.** Three intent variants,
one effect-path, uniformly governed. (Rationale in §4.2.)

**4. Admission — RESOLVED: plugin in the `observe` phase, not a core pillar.** The core exposes
a pluggable observe phase; it does not contain filtering policy. Firehose installs an aggressive
filter; chat installs pass-through; the core is identical.

**5. Soul — RESOLVED: plugin, one kind of state behind a generic handle.** The core knows "there
may be state to load before a span and persist after," as an optional state-handle slot; it does
not know what a soul *is*. This is also where per-state-kind concurrency policy lives.

**6. Capability-handle mechanism — RESOLVED in design, one ergonomic detail to confirm in code.**
The trait-object-granted-at-provision pattern (§6.2) is sound. The single thing to verify by
compiling is the wiring registry's ergonomics for storing heterogeneous handles without
`Any`-downcasting soup — a known-hard-but-solved Rust problem. First Wave-0 code target.

---

## 8. The plugin taxonomy (families)

Grouped by the resource each family owns; hierarchical IDs. A "deployment" is a plugin set.

| Family / slot | Owns | Representative plugin IDs |
|---|---|---|
| **provider** | the decision | `provider.llm.litellm`, `provider.llm.anthropic`, `provider.llm.llamacpp`, `provider.behaviortree`, `provider.rules` |
| **state** (incl. soul) | run/persistent state bytes | `state.memory`, `state.file`, soul/persona plugin |
| **context** | the turn's view (read-only over others) | `context.template`, `context.history`, `context.memory`, `context.compaction` |
| **memory** | durable cross-run facts | `memory.vector`, `memory.summarizer`, `memory.usermodel` |
| **capability** | the verbs the agent can invoke | `cap.registry`, `cap.shell`, `cap.fs`, `cap.http`, `cap.mcp`, `cap.state_write`, `cap.distribution` |
| **channel** | ingress/egress | `channel.cli`, `channel.http`, `channel.telegram`, `channel.discord`, `channel.slack`, `channel.game.socket`, `channel.ha.eventbus` |
| **execution** | where side effects run | `exec.local`, `exec.docker`, `exec.ssh`, `exec.serverless` |
| **scheduling** | turning triggers into observations | `sched.cron`, `sched.webhook`, `sched.eventbus` |
| **admission** (observe phase) | whether an observation becomes a goal | segmentation / heartbeat-filter plugins |
| **orchestration** | multi-agent shape | `orch.subagent`, `orch.delegate`, `orch.parallel` |
| **governance** (`govern` stage) | the allow/deny decision | `gov.allow`, `gov.policy`, `gov.approval`, `gov.secrets`, `gov.ratelimit`, `gov.audit`, `gov.idempotency` |
| **observation** (hooks only) | read-only telemetry | `obs.logging`, `obs.metrics`, `obs.tracing` |
| **skills** | polyglot, code-free extension | `skill.runner` (agentskills.io-format) |

---

## 9. The layered design model (rings)

Reliability and hackability are not a dial; they are different qualities at different layers,
joined by compiler-enforced boundaries. The rings exist for the builder's own future sanity:
experimenting must never threaten the parts depended upon.

- **Ring 0 — Core.** Small because small stable things don't break. The pipeline, loop, event
  stream. Invariants are types, not conventions: the dangerous path doesn't compile. The part
  trusted without re-reading.
- **Ring 1 — Plugins.** Isolated and swappable without fear. An in-process Rust plugin cannot
  violate a core invariant, bypass `govern`, or touch another family's state, because it was
  never handed the capability to. Safe by construction (for in-process; see §14.1 on RPC).
- **Ring 2 — Skills.** Zero-ceremony, polyglot, agentskills.io-shaped. Quick automation without
  a compile step. The layer touched most, thought about least; inherits skills written for the
  incumbents.

The tension dissolves because a mistake in an outer ring cannot reach an inner one — enforced
by the compiler, not by discipline.

---

## 10. Hardware safety boundary (deferred; contract preserved)

Pan is a **deliberative** brain. On a physical robot it must never be what decides whether an
action is *safe* — that belongs to a separate, simpler, faster, independently-trustworthy
safety controller (the reflex layer). This is recorded so Pan never makes a choice that
forecloses safety later; the safety project itself is **not built now**.

**Prior-art reality check.** This is not novel and the enforcer should not be invented here.
Industrial *functional safety* already does exactly this — safety-rated controllers / safety
PLCs under ISO 10218, ISO/TS 15066, IEC 61508, ISO 13849, sold certified by SICK, Pilz,
Keyence, and the robot makers. The "untrusted smart planner on top, trusted simple enforcer
below" pattern is the recognized safety-filter / runtime-assurance / simplex architecture. The
only individually-buildable contribution is the **source-agnostic contract** that lets an
arbitrary brain ride on top of certified enforcement *without the brain being in the safety
case*.

**Not-yet for Pan.** Pan's real near-term deployments — chat, NPCs, trend detection — cannot
physically harm anyone. The safety layer is needed the moment Pan drives actuators near people
and **not one day before**.

**Pan's three preserved obligations (near-free to keep now):**
1. **Coarse, cancellable intents** — goals, not joint torques; interruptible mid-flight.
2. **An out-of-band veto ingress** — the reflex layer can refuse/halt faster than the loop
   comes around; Pan acknowledges via the event stream and never overrides.
3. **Fail-quiet** — if Pan hangs, it emits no new intents; silence, by contract, means safe-stop
   below.

**Contract surface (for the eventual external project):** `command(intent_id, coarse_goal,
deadline)`, `cancel(intent_id)` downward; `vetoed(intent_id, reason)`, `completed(intent_id,
outcome)` upward; `safe_stop()` internal to the reflex layer and **not commandable by Pan**.
The interface is source-agnostic by design: it works equally for Pan, a ROS stack, or a human
with a joystick — and source-blindness is itself a safety property (a human commander is not
trusted more than an AI one).

---

## 11. Performance expectations

Stated as reasoned expectations to be confirmed by benchmark, not measurements.

**LLM deployments — Pan overhead is unmeasurable.** The provider call (hundreds of ms to
seconds) dominates Pan's own work (microseconds) by 3–4 orders of magnitude. The only metric
that matters is concurrency: how many spans can one process hold while blocked on model calls.
Rust async holds tens of thousands of parked spans on modest hardware; the bottleneck is the
model API's rate limits, never Pan. **Verdict: excellent, bounded by the provider.**

**Non-LLM deployments (NPC behavior trees, trend detection) — Pan's own speed is the story.**
Here per-decision overhead is the budget. Expected low *tens of microseconds* per decision,
meaning thousands–tens-of-thousands of decisions/sec per process. Two named costs with known
mitigations: **schema validation** in the `validate` stage (compile schemas at provision;
consider debug-only for trusted in-proc providers) and **event emission** (emit-to-channel /
process-off-thread from day one). One genuine unknown: whether the streaming substrate taxes the
discrete case — the one thing to benchmark before locking it in.

**The design is fast as a byproduct of correctness:** one effect-path (fewer branches), a tiny
core (little on the hot path), trait-object handles (vtable lookups), provider-as-plugin (the
expensive work is cleanly outside). Same discipline bought both correctness and speed.

**Discipline:** do not optimize until a real workload yields a profile (Wave 6). The
architecture leaves room to optimize exactly where needed; spending it early is how clean
designs accrete premature complexity.

---

## 12. Build roadmap (waves)

Each wave ends at a *runnable, useful* state. Build the boring correct version; reach a running
deployment early; add the next wave only when the current works end-to-end.

**Wave 0 — Core (no plugins).** The vocabulary types; the dispatch pipeline (typed,
non-bypassable); the four-phase stream-driven loop; the event stream (off-thread from day one);
plugin lifecycle; the capability-handle wiring registry (first real-code target — confirm one
read-only handle that refuses writes at compile time); the abandon-path (shared with the future
safety veto). *Exit:* a stub provider emits one `Invoke` through always-allow govern to a stub
capability and the event appears on the stream.

**Wave 1 — Walking skeleton (CLI agent).** `provider.llm.anthropic`, `cap.registry`,
`gov.allow`, `exec.local`, `cap.shell`, `obs.logging`, `channel.cli`, `state.memory`. *Exit:*
type a request in a terminal → model decides → shell command runs → reply printed → action in
logs. **Pan is now a usable agent.**

**Wave 2 — Make it real.** `state.file`, `cap.fs`, `cap.http`, **`cap.mcp` (highest-leverage —
inherits the MCP tool ecosystem)**, `cap.state_write`, `context.template`, `context.history`.
*Exit:* survives restart with memory of prior conversation; fetches a URL; calls an MCP tool.

**Wave 3 — Memory & the non-LLM honesty check.** `memory.vector`, `context.memory`,
`memory.summarizer`, `context.compaction`, `provider.litellm`, **`provider.behaviortree` (the
honesty check — built before it's needed, to prove the core never became LLM-only)**,
`provider.rules`. *Exit:* recalls a fact from days ago; the behavior tree drives a decision
through the same pipeline with zero LLM involvement.

**Wave 4 — Governance (before chat exposure).** `gov.policy`, `gov.approval`, `gov.secrets`,
`gov.audit`, `gov.ratelimit`, `gov.idempotency`, **`exec.docker`/`exec.ssh` (sandboxed
execution)**. *Exit:* a dangerous `Invoke` is gated by approval; a denial is refused and
audited; tools run in the sandbox, not on the host.

**Wave 5 — The assistant (Hermes/OpenClaw replacement).** `channel.telegram`/`discord`/`slack`,
`channel.http`, pairing/allowlist in `gov.policy`, soul/persona plugin + persona injection,
`sched.cron` + `sched.eventbus`, **the heartbeat-admission filter in `observe` (a tick is a
cheap observation that usually gets dropped, escalating to a full LLM decision only when
something changed)**, `skill.runner`, `cap.distribution`. *Exit:* message Pan from your phone;
it answers in persona, remembers you, runs a sandboxed tool with approval, and a heartbeat does
**not** wake the LLM unless a watched condition changed. **The replacement is running.**

**Wave 6 — Optimize & harden (only now).** Benchmark the discrete path through the streaming
machinery (the real unknown); compile schemas if `validate` is hot; confirm off-thread eventing
under a tight loop; tune memory thresholds. Optional/demand-driven: `provider.llamacpp`,
`orch.*`, `obs.metrics`/`obs.tracing`, more channels/capabilities.

### Dependency order at a glance

```
Wave 0  core ........ pipeline · loop · events · handles · lifecycle · abandon-path
Wave 1  CLI agent ... provider.anthropic · cap.registry · gov.allow · exec.local
                      · cap.shell · obs.logging · channel.cli · state.memory
Wave 2  real ........ state.file · cap.fs · cap.http · cap.mcp · cap.state_write
                      · context.template · context.history
Wave 3  memory ...... memory.vector · context.memory · memory.summarizer
                      · context.compaction · provider.litellm
                      · provider.behaviortree · provider.rules
Wave 4  governance .. gov.policy · gov.approval · gov.secrets · gov.audit
                      · gov.ratelimit · gov.idempotency · exec.docker
Wave 5  ASSISTANT ... channels · pairing · persona · sched.cron · sched.eventbus
                      · admission-filter · skill.runner · cap.distribution
Wave 6  optimize .... benchmarks · schema compile · tuning · optional extras
```

---

## 13. The reference deployment: Hermes/OpenClaw replacement

The target home deployment, mapped to the things people actually valued in the incumbents:

- **Model-swap freedom** → `provider.litellm`.
- **"Make it yours" persona** → soul/persona plugin + `context.template` (mostly user-edited
  markdown — Ring 2, not Rust).
- **"It remembers me"** (most-praised) → `memory.vector` + `context.memory` +
  `memory.summarizer`.
- **Lives in your chat apps** → `channel.telegram`/`discord`/`slack`/`http` + pairing/allowlist
  (inbound is untrusted — the OpenClaw-was-weak-here gap, now structural).
- **Always-on heartbeats** → `sched.cron`/`eventbus` + the admission filter (fixes "wakes the
  whole agent on a timer").
- **Acts on your behalf** → `cap.shell`/`fs`/`http`/`mcp` + `skill.runner`, scoped by
  `cap.distribution`, executed in `exec.docker`/`ssh` (sandboxed).
- **Safety the incumbent under-built** → the full `gov.*` stack: non-bypassable approval,
  durable audit, secret isolation.

**The thesis, as a checkable fact:** Wave 5 adds only ~5 genuinely assistant-specific plugins
(channels, persona, heartbeat-admission, skill-runner, distribution) on top of a baseline
(Waves 1–4) built for *any* deployment. The incumbent-equivalent is a **manifest plus five
plugins**, not a fork. If that holds at Wave 5, the boundary was drawn correctly.

---

## 14. Known seams & deferred problems

Named openly; each is bounded or deferred, none fatal.

**14.1 Two enforcement regimes (the RPC seam).** The "safe by construction" guarantee is a
property of the Rust type system and holds **only for in-process plugins**. Out-of-process
(RPC/MCP) plugins are enforced by the *runtime* instead: schema validation per frame, capability
tokens scoping requests, the OS/container sandbox, and the same `govern` gate. Strong, but a
weaker and different guarantee. Untrusted third-party code should always run out-of-process,
precisely because the in-process regime trusts the author's Rust.

**14.2 Soul/state concurrency.** "Load → mutate → persist" has a lost-update problem under
concurrent access (server-authoritative NPCs, multi-tenant). v1.0 stance: single-writer per
state-kind, decided at the state-handle slot. A real concurrency model (optimistic versioning, or
a state-owning actor) is a pre-multi-tenant requirement, not a today requirement.

**14.3 Plugin failure isolation.** In-process panic → contained at the plugin boundary, surfaced
as a typed event; the loop degrades rather than crashes. Out-of-process hang/crash → bounded by
timeout and supervision; a dead RPC plugin fails its dispatch stage, not the host. Open: the
per-family fail-open (observation: lose telemetry, continue) vs fail-closed (governance: deny on
failure) policy — itself a governance decision, likely in the `govern` contract.

**14.4 Precise claims.** "The unsafe path doesn't compile" is a guarantee about *structure*, not
*content*: the type system guarantees the dangerous path cannot be bypassed; it does not prove a
governance policy correct, a provider's translation faithful, or anything about out-of-process
code. Real, and more than the incumbents offer — but structural, not total.

**14.5 The one perf unknown.** Whether the streaming substrate taxes the discrete case (§11).
Resolved by one targeted Wave-6 benchmark before locking the loop's shape.

---

## 15. Appendix: the validated schema

The `Goal`/`ActionIntent` contract, validated by implementing three providers (LLM, behavior
tree, rules) against it in one file — the executable leak test. The crate accompanies this
report (`pan-schema/`). Key points reproduced here; full source in the crate.

**The leak test, made executable.** The pass condition: no provider sets a field that only makes
sense for another. The design decisions that make it pass:

1. `ActionIntent` is an enum; `Invoke` is one variant, not the whole type — de-privileges the
   LLM.
2. `Invoke.correlation` is `Option` — LLMs set it, behavior trees and rules decline it. A
   required `String` would force non-LLM providers to fabricate IDs: a leak.
3. `Express` is "emit to listeners," not "chat reply" — control-only providers never emit it.
4. `Conclude` replaces LLM `stop_reason` with a signal all three produce.

The decisive assertion holds all three providers in a `Vec<Box<dyn Provider>>` and drives each
through the same call. **That compiling is the thesis:** the core holds an LLM and a behavior
tree identically.

**Note vs. the original crate:** this report's settled design folds `Mutate` into `Invoke`
(§4.2) — the accompanying crate's `Mutate` variant is the transitional version and should be
removed when the crate is reconciled to v1.0 (a Wave-0 task). The three surviving variants are
`Invoke`, `Express`, `Conclude`.

```rust
// The core trait the loop knows. Note the absence: no messages, no system
// prompt, no temperature, no model name, no tokens. Those live inside whatever
// provider needs them.
pub trait Provider {
    fn id(&self) -> &str;
    fn decide(&self, goal: &Goal, ctx: &Context, caps: &[Capability]) -> Decision;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum ActionIntent {
    Invoke { capability: String, args: Value, #[serde(skip_serializing_if = "Option::is_none", default)] correlation: Option<String> },
    Express { body: String },
    Conclude { outcome: Outcome },
}
```

---

*Design settled. The next action is Wave 0 in an editor, starting with the capability-handle
wiring registry — the one piece whose ergonomics need real code to confirm. Everything else in
Wave 0 is well-understood construction.*
