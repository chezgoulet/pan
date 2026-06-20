# Pan — Build Manifest

**Goal of this document:** a dependency-ordered sequence from empty repo to a working
home assistant that replaces Hermes/OpenClaw. Built in waves; each wave ends at a
*runnable, useful* state, never a half-finished layer. Target end state (Wave 5) is the
personal-assistant deployment, but every wave before it is independently usable and worth
stopping at.

**Sequencing principle:** build the boring correct version of each piece, get to a running
deployment as early as possible, and only add the next wave when the current one actually
works end-to-end. Do not optimize until Wave 6.

**Status legend:** `[ ]` todo · `[~]` in progress · `[x]` done

---

## Wave 0 — Core (no plugins; the thing plugins plug into)

The three core pieces from the settled design, plus the plugin substrate. Nothing here is
a plugin; this is what Wave 1 plugs into. Until this compiles and a trivial hand-written
test can drive one fake provider through one fake capability, do not start Wave 1.

- [ ] `Goal` / `ActionIntent` / `Context` / `Capability` types — three-variant intent
      (`Invoke` / `Express` / `Conclude`), `Goal` carries `id` + `revision` for supersession.
- [ ] The dispatch pipeline: `resolve → validate → govern → execute → record` as typed
      stages where the unsafe path cannot be constructed. **This is the heart — get it right.**
- [ ] The loop: `observe → decide → enact → commit`, stream-driven (consumes an observation
      stream; a "run" is a span). The discrete case is the degenerate single-observation span.
- [ ] The event stream: ordered typed events, **emit-to-channel / process-off-thread** from
      day one (cheap struct onto a queue; consumer does serialization/persistence). Retrofitting
      this later is painful.
- [ ] Plugin lifecycle: `Register → Provision → Validate → Run → Cleanup` (Caddy-style),
      hierarchical IDs, explicit conflict = provision-time error (never last-wins).
- [ ] The capability-handle wiring registry: trait-object handles granted at provision.
      **The one piece flagged as needing real code to confirm ergonomics** — build the smallest
      version that injects one read-only handle and refuses at compile time to let it write.
- [ ] The abandon-path: cleanly discard an in-flight `Decision` whose goal was superseded.
      Shared mechanism with the (future, deferred) §14 safety veto — build once.

**Exit test:** a hand-written integration test drives a stub provider that emits one `Invoke`,
through an always-allow govern stage, to a stub capability, and sees the event on the stream.

---

## Wave 1 — Walking skeleton (first usable deployment: CLI agent)

The smallest plugin set that makes Pan do something real. End state: type into a terminal,
a model decides, a local tool runs, you see a reply. This is the moment Pan becomes a tool.

- [ ] `provider.llm.anthropic` (or your one chosen model) — first real provider.
- [ ] `cap.registry` — capabilities register here; pipeline `resolve` reads from it.
- [ ] `gov.allow` — trivial always-allow, so the `govern` stage runs. Replaced in Wave 4.
- [ ] `exec.local` — in-process execution so `Invoke` does real work.
- [ ] `cap.shell` — first actual verb (run a command). Proves the whole pipeline.
- [ ] `obs.logging` — structured logs on observation hooks. You are blind without this.
- [ ] `channel.cli` — stdin/stdout. The simplest possible human interface.
- [ ] `state.memory` — in-process, non-persistent state so `observe`/`commit` have a slot.

**Exit test:** in a terminal, "list the files in /tmp" → model emits `Invoke(cap.shell, …)` →
runs → reply printed → action visible in logs. **You now have a usable agent.**

---

## Wave 2 — Make it real (persistence, web, the MCP firehose)

Turns the toy into something you'd actually leave running. Two high-leverage moves here:
`cap.mcp` (inherits the entire existing MCP tool ecosystem for one plugin) and `state.file`
(state survives a restart).

- [ ] `state.file` — persist state/soul to disk. Settle the §13.2 concurrency stance here
      (single-writer to start is fine; document it).
- [ ] `cap.fs` — file read/write as a governed capability.
- [ ] `cap.http` — outbound web requests.
- [ ] `cap.mcp` — **bridge to any MCP server. Highest-leverage plugin in the whole manifest** —
      one plugin, and every existing MCP tool becomes available.
- [ ] `cap.state_write` — the unified soul/state-write capability (the `Mutate`-folds-into-`Invoke`
      decision made concrete). State changes are now governed like any other effect.
- [ ] `context.template` — prompt assembly from templates for the LLM provider.
- [ ] `context.history` — conversation history with pruning.

**Exit test:** restart Pan and it remembers the prior conversation; it can fetch a URL and
call at least one MCP-provided tool.

---

## Wave 3 — Memory & the non-LLM honesty check

The "it remembers me" layer (the single most-praised incumbent feature), and — critically —
the two non-LLM providers that keep the core honest. Build `provider.behaviortree` here
**even though the assistant doesn't need it**, as a living test that the core never became
LLM-only. If it can't emit the same `ActionIntent`s cleanly, something leaked; fix it now,
before more plugins assume LLM shape.

- [ ] `memory.vector` — thin client to a vector store (the Ragamuffin slot).
- [ ] `context.memory` — holds the read-only `MemoryQuery` handle, injects retrieved facts.
- [ ] `memory.summarizer` — condense old context into durable summaries.
- [ ] `context.compaction` — compress when the window fills.
- [ ] `provider.litellm` — one plugin, many models. Becomes your default provider (model-swap
      freedom is part of the self-hosted appeal).
- [ ] `provider.behaviortree` — **the honesty check.** No model. Must emit identical intents.
- [ ] `provider.rules` — second non-LLM provider; also the seed of the heartbeat-filter logic.

**Exit test:** Pan recalls a fact from a conversation days ago; the behavior-tree provider
drives a trivial decision through the *same* pipeline with zero LLM involvement.

---

## Wave 4 — Governance (the part the incumbents under-built)

Replace `gov.allow` with real gates. This is where a Pan assistant becomes meaningfully
*safer* than OpenClaw rather than just different — sandboxed execution, non-bypassable
approval, durable audit, secret isolation. Do this **before** exposing Pan over chat (Wave 5),
because the moment it's reachable by DM, inbound is untrusted.

- [ ] `gov.policy` — allow / deny / require-approval rules. Replaces `gov.allow`.
- [ ] `gov.approval` — human-in-the-loop confirmation for dangerous invokes.
- [ ] `gov.secrets` — resolve credentials without exposing them to plugins.
- [ ] `gov.audit` — durable record of every governed effect.
- [ ] `gov.ratelimit` — token/request/action ceilings.
- [ ] `gov.idempotency` — dedupe repeated effects.
- [ ] `exec.docker` (or `exec.ssh`) — **sandboxed** execution, replacing bare `exec.local`
      for anything touching real tools. Directly closes the credential-isolation gap.

**Exit test:** a dangerous `Invoke` (e.g. `cap.shell` doing an `rm`) is gated by approval;
a denied action is refused and audited; tools run inside the sandbox, not on the host.

---

## Wave 5 — The Hermes/OpenClaw replacement (home assistant)

Everything assistant-specific. Most of what's needed already exists from Waves 1–4; this wave
is really just **channels + persona + heartbeat-admission + skills**. This is your target
deployment.

- [ ] `channel.telegram` (and/or `channel.discord`, `channel.slack`) — live in your chat apps.
- [ ] `channel.http` — webhook/REST ingress for everything else.
- [ ] Pairing / allowlist rules in `gov.policy` — inbound DMs are untrusted; only paired
      senders reach the agent. (The OpenClaw-was-weak-here fix, now structural.)
- [ ] soul/persona plugin + `context.template` persona injection — the "make it yours"
      onboarding. Mostly user-edited markdown (Ring 2), not Rust.
- [ ] `sched.cron` + `sched.eventbus` — the heartbeat substrate.
- [ ] **admission/segmentation plugin in the `observe` phase** — the heartbeat filter: a tick
      is a *cheap observation that usually gets dropped* and only escalates to a full LLM
      decision when something changed. The fix for "wakes the whole agent every 30 min."
- [ ] `skill.runner` — execute agentskills.io-format skills (polyglot). Your low-barrier
      everyday-automation surface, and inherits skills written for the incumbents.
- [ ] `cap.distribution` — scope which capabilities are live for this deployment.

**Exit test:** message Pan from your phone via Telegram; it answers in persona, remembers you,
runs a sandboxed tool on request with approval, and a cron heartbeat quietly does NOT wake the
LLM unless a watched condition changed. **This is the Hermes/OpenClaw replacement, running.**

---

## Wave 6 — Optimize & harden (only now)

The design is performant by construction; do not touch any of this until a real workload
gives you a profile. Then fix only what the profile flags, in this likely order:

- [ ] Benchmark the discrete path running through the streaming machinery (the one genuine
      perf unknown — confirm the general-case substrate didn't tax the simple case).
- [ ] Compile JSON schemas at provision time if `validate` shows up hot.
- [ ] Confirm off-thread eventing holds under a tight non-LLM loop.
- [ ] Tune memory retrieval / compaction thresholds against real conversation volume.

Optional, demand-driven:

- [ ] `provider.llamacpp` — local models.
- [ ] `orch.subagent` / `orch.delegate` — multi-agent specialists, only if wanted.
- [ ] `obs.metrics` / `obs.tracing` — when you want dashboards.
- [ ] additional channels / capabilities as needs arise.

---

## At-a-glance dependency order

```
Wave 0  core ............... pipeline · loop · events · handles · lifecycle
Wave 1  CLI agent ......... provider.anthropic · cap.registry · gov.allow · exec.local
                            · cap.shell · obs.logging · channel.cli · state.memory
Wave 2  persistent+tools .. state.file · cap.fs · cap.http · cap.mcp · cap.state_write
                            · context.template · context.history
Wave 3  memory+honesty .... memory.vector · context.memory · memory.summarizer
                            · context.compaction · provider.litellm
                            · provider.behaviortree · provider.rules
Wave 4  governance ........ gov.policy · gov.approval · gov.secrets · gov.audit
                            · gov.ratelimit · gov.idempotency · exec.docker
Wave 5  ASSISTANT ......... channel.telegram/discord/slack · channel.http · pairing
                            · persona · sched.cron · sched.eventbus · admission-filter
                            · skill.runner · cap.distribution
Wave 6  optimize .......... benchmarks · schema compile · tuning · optional extras
```

## The thesis, restated as a checklist fact

The Hermes/OpenClaw replacement (Wave 5) adds only ~5 genuinely assistant-specific plugins
(channels, persona, heartbeat-admission, skill-runner, distribution) on top of a baseline
(Waves 1–4) you would build for *any* deployment. The incumbent-equivalent is a **plugin
manifest plus five plugins**, not a special build. If that holds true when you reach Wave 5,
the core/plugin boundary was drawn correctly.
