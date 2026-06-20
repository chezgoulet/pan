# Pan — Core + Plugin Architecture (Synthesis v0.2)

**Status:** Draft v0.2. Soul-centric design with four enforced architectural boundaries, plus a layered design model.
**Thesis:** One irreducible core. Everything else is a plugin. Same binary, different plugin sets, drives a chat gateway, game NPCs, and headless trend detection.

**Why this exists:** to build a tool I will depend on, and to build it well. Adoption is not a goal. If others find it useful, good; if not, also good. Every decision below is justified by *correctness and my own future sanity*, not by what would make the project popular. That single constraint makes the architecture freer to be uncompromising — there is no countervailing pressure to lower a barrier or court a contributor base.

This revision keeps what the working design got right (soul file as first-class, Caddy lifecycle, sidecar/queryable-world-state boundary, Ragamuffin as external memory) and corrects four places where the lifecycle-hook model quietly reintroduced coupling we had already designed out.

**Language: Rust.** The boundaries below are not conventions enforced by review — the *shape* of the dangerous path is enforced by the type system and ownership model. A capability that has not passed the `govern` stage cannot be constructed in a form `execute` accepts; an in-process plugin that was not granted a `MemoryQuery` handle cannot reach memory. The point is not to impress anyone — it is that a guarantee held by the compiler is a guarantee I don't have to hold in my head at 2am. **Precise claim:** the type system guarantees the dangerous *path* cannot be bypassed; it does **not** prove a governance *policy* is correct, that a provider's translation is faithful, or anything about out-of-process plugins (see §13.1). (Code below is illustrative Rust-shaped pseudocode; trait names and signatures will firm up in v0.3.)

---

## 0. The four boundaries (non-negotiable for v0.2)

1. **Provider is a plugin, not core.** The core carries no prompt, no token format, no tool-call convention. The LLM-shaped request/response is the contract of *one* provider plugin, not the core API.
2. **The side-effecting path is a fixed pipeline, not peer hooks.** `resolve → validate → govern → execute → record`. Plugins fill stages; no plugin can reorder or skip one. Hooks exist only for observation.
3. **No shared mutable god-state.** Plugins receive scoped, typed accessors — read-only handles to what they query, write access only to the resource they own.
4. **Admission precedes the loop.** An `Input` is triaged into "becomes a run" or "dropped/deferred" *before* any provider call. The firehose and real-time cases require this.

Each boundary is enforced structurally (a handle that isn't granted, a stage that can't be skipped, a shape that won't validate) — never by asking implementers to be disciplined.

---

## 1. Lessons carried forward

From the prior analysis, kept intact:

- **Hermes** — provider abstraction and the markdown skills mechanism are right; the 200-line kitchen-sink config and the loop being fused to TUI/gateway/cron are the anti-patterns to avoid.
- **OpenAI Agents SDK** — one `Run()` entry point and a ~20-line deterministic turn loop are the target; bundled guardrails and the identity/config/registry-conflating Agent object are not.
- **Anthropic patterns** — system prompt first-class, native tool_use as the cleanest primitive.
- **Vercel AI SDK** — provider-agnostic core, simplest-possible multi-step abstraction, per-tool error handling.
- **Caddy** — self-registration by hierarchical ID, minimal module interface, and the **Register → Provision → Validate → Run → Cleanup** lifecycle. Its weaknesses (compile-time only, last-registration-wins) are explicitly designed around below.

New, from the boundary critique:

- A turn is not always discrete (voice), goals do not always arrive one-at-a-time (firehose), there is not always one tenant (SaaS), governance is not always in-band (robotics), and no-replay is a *core stance* not a plugin detail (audit/replay). The core must take an explicit position on each rather than inherit the single-user assumption from the reference projects.

---

## 2. The Pan core

### 2.1 What the core is

The core owns five things and nothing else:

1. **Soul handling** — load opaque identity+state bytes before a run, persist mutated bytes after. The core does not parse or understand soul schema.
2. **Admission** — decide whether an `Input` becomes a run (see §5).
3. **The loop** — assemble context (via plugins) → ask the provider (a plugin) for action-intents → run each side-effecting intent through the dispatch pipeline → collect mutations.
4. **The dispatch pipeline** — the fixed, non-bypassable `resolve → validate → govern → execute → record` sequence.
5. **The event stream** — an ordered, typed record of everything that happened, with a defined stance on replay (see §6).

### 2.2 The internal vocabulary (why the provider is out)

The loop speaks only this:

> goal + assembled context + available capabilities → zero or more action-intents

An **action-intent** is provider-agnostic: a named capability plus arguments, or a soul mutation, or "respond with this content." It is *not* a tool_call. The `provider.llm` plugin translates LLM tool_use blocks into action-intents; a `provider.behaviortree` plugin emits the same action-intents from a tree tick; a `provider.rules` plugin emits them from rule evaluation. The core cannot tell which produced them.

### 2.3 Core API surface

```rust
type Pan struct {
    Provider   Provider      // a plugin satisfying the Provider slot. required.
    Soul       SoulStore     // opaque load/persist. if nil, no-op in-memory.
    Admission  Admitter      // triage. if nil, AdmitAll.
    Plugins    *Registry     // hierarchical-ID registry (Caddy-style).
    Events     EventSink     // ordered event stream. if nil, discard.
}

func (p *Pan) Run(ctx context.Context, in Input) (*Result, error)

type Input struct {
    Source  string            // "chat", "cron", "game.event", "sensor", ...
    Payload map[string]any    // arbitrary; admission + context plugins interpret it
}

type Result struct {
    Intents   []ActionIntent  // what the run decided to do
    SoulState []byte          // opaque, persisted by the SoulStore
    Events    []Event         // the run's ordered record
}
```

Note what is **absent** versus the prior draft: no `Messages`, no `System`, no `Tools`, no `Temperature`, no `StopReason` in the core. Those moved into the provider plugin's own contract.

### 2.4 The Provider slot (now a plugin)

```rust
// Core knows only this:
type Provider interface {
    ID() string
    Decide(ctx context.Context, g Goal, c Context, caps []Capability) ([]ActionIntent, error)
}
```

The LLM implementation owns the chat-shaped detail internally:

```rust
// inside plugin provider.llm — NOT in core
type completionRequest struct {
    Model, System string
    Messages      []Message
    Tools         []ToolSchema
    MaxTokens     int
    Temperature   float64
}
// provider.llm maps Goal+Context+caps -> completionRequest,
// calls the model, maps tool_use -> []ActionIntent.
```

This is the single most important change: the chat model is now one strategy, not the universal contract.

---

## 3. The dispatch pipeline (replaces peer AfterResponse hooks)

Every side-effecting action-intent passes through one pipeline. The **sequence is core**; the **stage implementations are plugins**.

```
resolve  -> validate -> govern  -> execute -> record
(name to  (args vs    (policy,   (in-proc   (event
 capability schema)    secrets,   or RPC)     stream)
 binding)             rate, audit)
```

- No plugin can register "an AfterResponse hook that executes a tool." Execution only happens at the `execute` stage, and `execute` is only reached after `govern` returns allow. This structurally removes the dispatch/policy entanglement.
- A governance plugin only ever receives the `govern` call with a typed decision interface (`Allow / Deny / RequireApproval`). It cannot perform execution because it is never handed the executor.
- `resolve` is where in-process vs RPC/MCP is decided, invisibly to the loop — the capability's registration declares its transport.

Hooks (`BeforeContext`, `AfterDecide`, `OnEvent`) still exist, but are **observation only**: they receive read-only views and cannot mutate intents or bypass stages. Logging, metrics, and tracing live here.

---

## 4. Plugin model: scoped accessors, not shared LoopState

The prior `LoopState` struct (every hook reads/writes every field, plus `PluginState map[string]any`) is replaced by **capability handles granted at wiring time**.

- A context plugin that needs memory receives a `MemoryQuery` handle — read-only, typed. It cannot write memory and holds no reference to the memory plugin itself.
- Only the loop's mutation-collection step can append to the run's mutation set. Plugins *propose* mutations as action-intents; they do not write them directly.
- Per-plugin scratch is private to that plugin, not a shared map other plugins can read.

```rust
// granted to context-family plugins; nothing else can be done with it
type MemoryQuery interface {
    Retrieve(ctx context.Context, q Query) ([]Fact, error) // read-only
}
```

**The sync-vs-async rule, made precise:**
- Synchronous cross-family *reads* → explicit granted handle (context assembling a turn may read memory *now*).
- Asynchronous cross-family *reactions/writes* → emitted on the event stream (memory updating *because* something happened).
- No plugin ever holds a write handle to a resource another family owns.

### Caddy lifecycle, adopted

Every plugin: `Register` (by hierarchical ID) → `Provision` (deps, config) → `Validate` → `Run` → `Cleanup`. Hierarchical IDs (`memory.vector.ragamuffin`, `provider.llm.litellm`) give organization and conflict resolution. **Conflict resolution is explicit, not last-wins:** two plugins claiming the same slot+ID is a Provision-time error, not a silent override.

---

## 5. Admission (new core responsibility)

Before any provider call, `Admitter.Admit(Input) -> (Goal, bool)` decides whether the input is worth a run.

```rust
type Admitter interface {
    Admit(ctx context.Context, in Input) (Goal, bool, error)
}
```

- Chat gateway: nearly everything admits.
- Firehose/trading: most events are filtered out cheaply; only threshold-crossing events become goals. This is where you avoid paying full-loop cost per event.
- Voice: admission can coalesce partial inputs into one evolving goal.

`AdmitAll` is the default so simple deployments ignore it entirely. But it exists in the core because retrofitting a pre-loop filter after the loop assumes one-goal-at-a-time is expensive.

---

## 6. The event stream stance (was an unstated assumption)

The prior drafts inherited "no-replay" silently from OpenClaw. v0.2 makes it a declared core property with two modes:

- **Ephemeral** (default): ordered, observation-only, not persisted. Cheapest; fine for NPCs and chat.
- **Durable/replayable** (opt-in): every event persisted such that a run can be deterministically reconstructed. Required for audit-grade deployments and for trajectory generation (the Hermes training use case).

The core defines the event schema and ordering guarantee once; the persistence/replay behavior is a plugin behind the `EventSink` slot. Choosing replay is a config choice, not a fork.

---

## 7. Plugin families (the taxonomy, reconciled)

Hierarchical IDs, grouped by the resource each family owns. A "feature" (NPC cognition, autonomous agent, trend detector) is a plugin *set*.

| Family / slot | Owns | Representative plugin IDs |
|---|---|---|
| **provider** | the decision | `provider.llm.litellm`, `provider.llm.anthropic`, `provider.llm.llamacpp`, `provider.behaviortree`, `provider.rules` |
| **soul** | identity+state bytes | `soul.file`, `soul.schema` (validation only), `soul.store.remote` |
| **context** | the turn's view (read-only over others) | `context.template`, `context.fewshot`, `context.history`, `context.memory` (holds MemoryQuery) |
| **memory** | durable cross-run facts | `memory.vector.ragamuffin`, `memory.summarizer`, `memory.usermodel` |
| **capability** | the verbs the agent can invoke | `cap.registry`, `cap.shell`, `cap.fs`, `cap.http`, `cap.mcp`, `cap.distribution` (which subset is live) |
| **channel** | ingress/egress | `channel.chat.*`, `channel.game.socket`, `channel.ha.eventbus`, `channel.sensor.*` |
| **execution** | where side effects run | `exec.local`, `exec.docker`, `exec.ssh`, `exec.serverless` |
| **scheduling** | turning triggers into Inputs | `sched.cron`, `sched.webhook`, `sched.eventbus` |
| **orchestration** | multi-agent shape | `orch.subagent`, `orch.delegate`, `orch.parallel` |
| **governance** (pipeline `govern` stage) | the allow/deny decision | `gov.policy`, `gov.secrets`, `gov.ratelimit`, `gov.audit`, `gov.idempotency` |
| **observation** (hooks only) | read-only telemetry | `obs.logging`, `obs.metrics`, `obs.tracing` |

Two reclassifications worth noting against the uploaded doc:
- `agent.sandbox` and `rate_limit`/`auth` are **not** `AfterResponse`/`BeforePrompt` hooks. They are `execution` and `governance` respectively, on the dispatch pipeline, where they cannot be bypassed.
- `output.tools` / `output.mutations` are not hooks either; they are how `provider.llm` produces action-intents, then the pipeline takes over.

---

## 8. What does not belong in Pan (kept from the working design, endorsed)

World simulation — faction AI, economy, world events — is **not** in Pan. It runs on its own tick loop as a service Pan *queries* during context assembly via a narrow read-only API. This is the cleanest statement of the resource-ownership rule: Pan owns identity→decision→effect; the world owns world state; they meet only through a query handle.

Likewise: Pan is not a memory store (it queries Ragamuffin), not a game engine (it connects over a local socket), not a TUI (library + CLI frontend), not opinionated about soul schema.

---

## 9. Deployment profiles (same binary, different plugin set)

| Family | Chat gateway | Game NPC | HA trend detector |
|---|---|---|---|
| provider | `provider.llm.*` | `provider.llm` or `provider.behaviortree` | `provider.rules` / small model |
| soul | full + user model | per-character soul | per-appliance baseline soul |
| context | full | light + world-state query | windowed sensor history |
| memory | `memory.vector.ragamuffin` | same, filtered by NPC ID | rolling baselines |
| capability | broad | game-action verbs | query/alert verbs |
| channel | many chat platforms | `channel.game.socket` | `channel.ha.eventbus` |
| execution | docker/ssh/serverless | `exec.local` | `exec.local` |
| scheduling | cron + webhooks | game tick / event | thresholds + cron |
| orchestration | multi-agent | single | single |
| governance | full (pairing, approval) | minimal | audit + ratelimit |
| admission | admit-most | per-event | aggressive filter |
| events | ephemeral | ephemeral | durable (audit) |

The core binary is identical in every column.

---

## 11. The layered design model

Reliability and hackability are **not** a dial to slide between. They are different qualities that live at different layers, joined by hard, compiler-enforced boundaries. Assigning them deliberately is what lets the same tool be rock-solid where it matters and frictionless where it doesn't. The three rings exist for *my own future sanity* — so that experimenting never threatens the parts I depend on.

**Ring 0 — The Core (small, stable, rarely touched).**
The five responsibilities (soul, admission, loop, dispatch pipeline, event stream). Small because small stable things don't break. Exhaustively tested, versioned with care, changed rarely and deliberately. I don't fork it and I don't need to. Rust does real work here: invariants are types, not conventions, so the dangerous path doesn't compile. This is the part I want to be able to trust without re-reading it.

**Ring 1 — Plugins (isolated, swappable without fear).**
Providers, channels, memory clients, capabilities, governance policies. The point of isolation is that I can swap and experiment here without any risk of corrupting the core. An in-process Rust plugin cannot violate a core invariant, bypass `govern`, or touch another family's state, because it was never handed the capability to — the type system contains it, not my own vigilance. This is where most of the building and tinkering actually happens, and it's safe by construction. (Out-of-process plugins are a different, weaker enforcement regime — see §13.1.)

**Ring 2 — Skills (zero-ceremony automation).**
Markdown-plus-any-executable. Easy on purpose: quick automations shouldn't require ceremony or a compile step. Polyglot because the right tool for a given skill might be Python, a shell script, or anything else. Conforming to the agentskills.io shape is a convenience — it means skills written elsewhere mostly drop in, and mine could travel if I ever wanted — but it earns its place only where it stays simple, not as an obligation. This is the layer I touch most often and think about least.

### 11.1 Why the tension dissolves

The two qualities stop fighting because they live in different rings, and the boundaries between rings are enforced by the compiler rather than by discipline:

```
Ring 0  Rust core      unsafe PATH cannot be built; the part I depend on, kept small
   │     (hard boundary: granted capabilities only)
Ring 1  Rust plugins   swap and experiment freely; boundaries compiler-enforced
   │     (hard boundary: SKILL.md contract + sandboxed execution)
Ring 2  Skills         zero-ceremony, polyglot, agentskills.io-shaped
```

The one-line version: **the core is small and provably un-bypassable so I can stop worrying about it; the plugin ring is isolated so I can experiment without fear; the skills ring is frictionless so everyday automation stays effortless.** Each ring optimizes for a different relationship between me and the code, and the hard boundaries mean a mistake in an outer ring can't reach an inner one.

---

## 12. Open questions for v0.3

- **Goal/ActionIntent concrete schema** — the exact typed shape that an LLM provider *and* a behavior-tree provider can both satisfy without one cosplaying the other. This is the make-or-break contract.
- **Admission ↔ loop handoff for streaming** — how a coalescing admitter (voice) feeds an evolving goal into a loop that currently assumes a goal is fixed at run start.
- **Multi-tenant isolation** — both reference projects are single-user; the SaaS profile needs per-tenant soul/memory/governance scoping that v0.2 has not specified.
- **Out-of-band governance** — the robotics reflex-veto case: can a governance plugin halt an in-flight execution on a faster control path, or is `govern` strictly pre-execution?
- **Handle granting mechanism in Rust** — the concrete pattern (trait-object injection at Provision time) that grants `MemoryQuery` to context plugins without handing over the memory plugin itself.

---

## 13. Known seams & deferred problems

This section exists because naming our own weaknesses is the highest-trust move in a design review. None of these are fatal; each is either bounded or explicitly deferred.

### 13.1 Two enforcement regimes, not one (the RPC seam)

The Ring 1 "safe by construction" guarantee is a property of the **Rust type system**, which means it holds **only for in-process plugins**. The moment a plugin runs out-of-process (the RPC/MCP path from §3.1, used for heavy or untrusted work), ownership and borrow-checking guarantee nothing about it — it can hold arbitrary state, call anything it can reach, and return dishonest outputs. Pan therefore has **two enforcement regimes, stated openly:**

- **In-process plugins** — enforced by the type system. Capability handles, no shared state, non-bypassable pipeline. The strong claims apply here.
- **Out-of-process plugins** — enforced by the *runtime*, not the compiler: schema validation on every frame, capability tokens scoping what the plugin may request, the OS/container sandbox bounding what it can touch, and the same `govern` stage gating any effect it asks Pan to perform. Strong, but a different and weaker guarantee than the compiler.

The honest summary: **the compiler protects the host from in-process plugins; the sandbox-plus-schema layer protects the host from out-of-process plugins.** I should always know which regime a given plugin falls under. Untrusted third-party code should always run out-of-process precisely because the in-process regime trusts the author's Rust.

### 13.2 Soul concurrency

§2.1 describes soul as opaque bytes the core loads, mutates, and persists. That "load → mutate → persist" cycle has a **lost-update problem** the moment two runs touch the same soul concurrently — server-authoritative NPCs (§7) and multi-tenant deployments both hit this. v0.2 stance: **single-writer per soul for now.** v0.3 must choose a real concurrency model (optimistic versioning with conflict rejection, or a soul-owning actor serializing writes). Flagged, not solved.

### 13.3 Plugin failure isolation

An enterprise-reliability pitch must answer "what happens when a plugin panics, hangs, or returns garbage" — and v0.2 was silent, which reads as unconsidered. The intended stance, to be specified in v0.3:

- **In-process plugin panic** → contained at the plugin boundary (`catch_unwind`-style), surfaced as a typed error on the event stream; the loop degrades (skips that plugin's contribution) rather than crashing.
- **Out-of-process plugin hang/crash** → bounded by timeout and process supervision; a dead RPC plugin fails its dispatch stage, not the host.
- **Open question:** per-family failure policy — which plugins are *fail-open* (observation: lose telemetry, continue) vs *fail-closed* (governance: deny on failure, never continue). This is itself a governance decision and likely belongs in the `govern` contract.

### 13.4 Precise-claims correction

"Provably reliable" has been narrowed throughout to its defensible form: the type system guarantees the **dangerous path cannot be bypassed**; it does **not** prove a governance policy is correct, that a provider's translation is faithful, or anything about out-of-process code. The guarantee is real and worth having — but it is a guarantee about *structure*, not about *correctness of content*.

### 13.5 Build order, not market timing

There is no adoption clock. The only sequencing that matters is internal correctness: the one item that can invalidate a core thesis (the `Goal`/`ActionIntent` schema, §12) is built and validated first, against both an LLM provider and a non-LLM provider at once. Everything in this section is a bounded or deferred risk, not a blocker — the soul-concurrency and failure-isolation stances (§13.2, §13.3) get pinned down before the first deployment that can actually trigger them (server-authoritative NPCs, multi-tenant), and not before. Build the parts I'll depend on soonest, to the standard I want to depend on, in that order.

---

## 14. Hardware safety boundary (Pan's side of the contract)

Pan is a **deliberative** agent: it decides *what to do* at human-decision cadence. On a physical robot it must never be the system that decides *whether it is safe to keep doing it*. That responsibility belongs to a separate, simpler, faster, independently-trustworthy safety controller (the "reflex layer"), specified as its own project and stubbed here as **external, TBD**. This section defines only **Pan's half of the contract** — the properties Pan must guarantee so that such a controller can exist beneath it. The safety controller is not built into Pan, and must not be, because a safety layer earns trust by being simpler than the thing it guards, and absorbing it into Pan would give it Pan's entire complexity as its trusted computing base.

This is the resource-ownership rule (§8) applied to physical safety: **Pan commands; the safety layer owns the veto.** Pan is never handed the capability to be unsafe, because the thing that can stop the robot is outside Pan's reach.

### 14.1 Why this is a boundary, not a feature

The reflex layer wants properties Pan cannot and should not have: hard real-time determinism, no GC/allocator pauses in the hot path, often a different language or a separate microcontroller/safety-PLC, and a hardware watchdog able to cut power independent of the CPU running Pan. Pan's async runtime and heap allocation are the *right* choices for deliberation and *disqualifying* for a safety controller. One codebase cannot satisfy both constraint sets; trying produces something mediocre at reasoning and unsafe as a safety system. Two projects, one contract.

### 14.2 What Pan owns (its three guarantees)

**1. Coarse, cancellable intents.** Pan emits goals, not actuation. An `ActionIntent::Invoke` aimed at hardware must be at the granularity of "place mug in cupboard," never "set joint to 47.3°." A lower real-time planner decomposes coarse intents into motion. If Pan's intents reach joint/torque granularity, a language-model-class provider has entered the control loop — forbidden. Every hardware-bound intent must also be **interruptible mid-flight**: a vetoed or cancelled intent unwinds cleanly without corrupting Pan's own run state.

**2. An out-of-band veto ingress.** The safety layer can refuse or halt an in-flight action faster than Pan's loop comes back around. Pan therefore exposes an **asynchronous event-stream ingress** the safety controller writes to ("I stopped X / X was refused"), outside the normal `resolve → validate → govern → execute → record` pipeline. Pan consumes these to keep its world-model coherent when overruled. This is the concrete resolution of the §13 robotics open question: `govern` remains pre-execution, and the veto ingress is the separate, post-dispatch, interrupt path. Pan never gets to override a veto; it only acknowledges it.

**3. Fail-quiet on Pan's own death.** If Pan hangs, panics, or falls behind, the command interface defaults to **emitting no new intents**. Pan guarantees silence rather than stale commands. The reflex layer, by contract, interprets silence as "hold position / safe-stop." A hung *deliberative* provider must therefore cause a safe stop, never continued motion on stale intent — making this the most safety-critical instance of the fail-closed policy in §13.3.

### 14.3 What Pan explicitly does NOT own

Balance, collision avoidance, force/torque limiting, proximity-stop, emergency-stop, and any control loop above roughly human reaction speed. These live entirely in the reflex layer. Pan does not implement them, does not have a plugin slot for them, and must be architecturally prevented from being on their critical path. If a behavior must be reliable in milliseconds, it is not a Pan concern.

### 14.4 The contract surface (to be satisfied by the external safety project)

A minimal, language-agnostic interface, stated as obligations on each side:

| Direction | Message | Semantics |
|---|---|---|
| Pan → reflex | `command(intent_id, coarse_goal, deadline)` | A cancellable request. Reflex MAY refuse. |
| Pan → reflex | `cancel(intent_id)` | Pan withdraws a still-pending/in-flight intent. |
| reflex → Pan | `vetoed(intent_id, reason)` | Reflex refused or halted; Pan updates world-model. |
| reflex → Pan | `completed(intent_id, outcome)` | Reflex finished decomposing/executing. |
| reflex (internal) | `safe_stop()` | Triggered by reflex on hazard OR on Pan silence past a timeout. Not commandable by Pan. |

**Reflex-side obligations (external project must guarantee):** safe-stop is reachable without Pan; silence-past-timeout ⇒ safe-stop; veto is always available and always wins; the interface is simple enough to be independently auditable/certifiable.

**Pan-side obligations (this project guarantees):** intents are coarse and cancellable; the veto ingress is honored and never overridden; Pan fails quiet. These three are the only hardware-safety responsibilities that belong inside Pan.

### 14.5 Build stance (and a prior-art reality check)

**This is not a novel problem, and the enforcement layer should not be invented here.** The reflex layer described above already exists as a mature, mandatory field: industrial *functional safety* — safety-rated controllers and safety PLCs governed by ISO 10218, ISO/TS 15066 (the cobot force/speed limits are literally tabulated there), IEC 61508, and ISO 13849, sold certified by SICK, Pilz, Keyence, and the robot makers. The architectural pattern of "untrusted smart planner on top, trusted simple enforcer below" is also already recognized in research as the safety-filter / runtime-assurance / simplex pattern (control barrier functions, safety shields). Pan invents none of this.

What classical functional safety assumes — a deterministic, legible, bounded commander (a PLC program or a human at a pendant) — is exactly what an agentic brain violates. So the genuinely underspecified, individually-buildable contribution is **not** the enforcer; it is the *clean, source-agnostic contract* (§14.4) that lets an arbitrary agent brain ride on top of certified-or-certifiable enforcement *without the brain being inside the safety case*. If the reflex layer is ever built, the first move is to **wrap or delegate to certified hardware**, not to hand-roll functional safety — re-implementing and certifying that from scratch is a multi-year specialized effort an individual builder will not match.

**For Pan as it actually exists, this is a not-yet problem.** Pan's real near-term deployments — chat gateway, game NPCs, Home Assistant trend detection — *cannot physically harm anyone*. A bad NPC decision is a gameplay bug, not a safety incident. The safety layer is needed the moment Pan drives actuators near people and **not one day before** — a day that may be years out or may never come.

Therefore: do **not** build the reflex layer, and do not let its absence block anything. The only thing worth doing now is the near-free part — keep §14.2's three Pan-side obligations (coarse cancellable intents, async veto ingress, fail-quiet) as live constraints so Pan never makes an architectural choice that would make safety impossible to bolt on later. That preserves the option at almost zero cost. Actually building (or buying) enforcement waits for a specific robot, a specific environment, and a specific hazard — and starts with evaluating certified hardware before writing a line of safety code. Pan's job is to be a brain that an independent, already-existing class of spinal cord can always overrule.
