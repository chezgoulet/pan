# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Pan is

Pan is an agent harness in Rust. One core, driven by different plugin sets, powers a
chat assistant, game-NPC brains, and headless trend detection. **The central design
decision: the reasoning model is a plugin.** The core contains no prompt, no token
format, and no tool-call convention — which is exactly what lets a behavior tree or a
rules engine stand in for an LLM against the same contract, without any of them
pretending to be the others (`pan-core/src/providers.rs` is the "honesty check" for
this). Anything chat-shaped (endpoints, prompts, models) is confined to a provider
module and never leaks into the core vocabulary.

## Workspace

- **`pan-core/`** — the irreducible Ring 0 substrate: vocabulary, dispatch pipeline,
  loop, event stream, plugin/capability lifecycle. Settled ("Wave 0"). No real
  plugins live here, only stubs needed to drive the core end-to-end.
- **`pan-daemon/`** — the Soul Protocol server (`pan` binary). Speaks the protocol
  over TCP loopback NDJSON, hosts souls, decides, ships decisions back to the host.

Per-crate `README.md`s are detailed and current — read them before deep work.

## Commands

Run from the repo root (workspace-aware) unless noted. CI runs `cargo fmt`/`cargo
clippy` with the working directory set to `pan-core`.

```sh
cargo build                                    # whole workspace
cargo test                                     # all tests
cargo test -p pan-core                          # one crate
cargo test --test wave_0_exit                   # one integration test file
cargo test superseded_decision                  # tests matching a name substring
cargo fmt --all --check                         # format gate (CI: -D warnings)
cargo clippy --all-targets -- -D warnings       # lint gate
./pan-core/verify.sh                            # tests + the compile-fail guarantees (see below)

./target/debug/pan serve --port 40707           # run the daemon (also honors $REACHLOCK_PAN_PORT; default 40707)
./target/debug/pan check-conformance            # validate bundled fixtures, exit 0/1
```

## Core architecture (pan-core)

The dispatch pipeline is a **non-bypassable type-state chain**:

```
Resolved --validate--> Validated --govern--> Governed --execute--> Effected
                                                ^ the ONLY source of a Governed is govern() returning Allow
```

`Governed` has a private field and no public constructor, and `Pipeline::execute`
accepts only a `Governed` — so **there is no expressible way to execute an ungoverned
effect.** The same pattern protects memory: a `MemoryQuery` grant has no write method
and its concrete handle type is private, so a read grant cannot be upgraded to a
writer. These are not comments — `pan-core/tests/compile-fail/` holds programs that
attempt each bypass together with the rustc error they must produce
(`governed_bypass.rs`→E0451, `handle_write.rs`→E0599, `handle_downcast.rs`→E0412,
`invoker_no_scope_injection.rs`→E0061). **If any compile-fail program starts
compiling, a core boundary has regressed;** `verify.sh` checks this half. The exact
error *code* is a secondary hint — it can drift across toolchains (handle_downcast
now reports E0425), so `verify.sh` treats a differing-but-still-failing code as a
warning and only fails on a bypass that compiles or a failure with no compiler
error at all. Run it with cargo on PATH: `PATH="$HOME/.cargo/bin:$PATH"` (rustup
shim; not on PATH by default here).

Vocabulary lives in `schema.rs`: `Goal` / `Context` / `Capability` / `ActionIntent`
/ `Scope`. `ActionIntent` has exactly three variants — `Invoke` / `Express` /
`Conclude`. There is deliberately no `Mutate`: **a state write is an `Invoke` of a
state-write capability**, nothing more.

**Governance is origin-aware.** `Governor::govern` takes a `Scope` (who is asking —
persona, skill, meta-agent); every `EffectRequest` carries one, so there is no
unscoped effect path. `pipeline::ScopedGovernor` is the reusable policy shape
(origin → allowed capability-id prefixes, deny-by-default); `AllowAll` ignores
scope. See **`docs/decisions/0001-scope-invoker-components.md`** (ADR 0001), the
architecture record binding the extended buildout — read it before touching
`govern`, the loop's scope, skills, or component wiring.

Two more core modules implement ADR 0001:
- **`invoker.rs`** — `ScopedInvoker`: the *only* governed surface a skill/sub-agent
  holds. Its `invoke(capability, args)` routes through the full pipeline under a
  bound scope; the scope is not a parameter, so a holder cannot widen its own
  authority. This is the invocation analogue of the read-only `MemoryQuery` handle.
  A future Python-subprocess bridge is a thin transport over this trait.
- **`components.rs`** — `ComponentRegistry`: per-family factory tables
  (`Provider`/`Governor`/`Executor`) keyed by config id, conflict-is-error. This is
  the **Component** wiring mechanism (in-process trait impls selected by
  `Agent.toml`) — distinct from the **Plugin** mechanism (`plugind.rs`, out-of-process
  Wasm). Don't conflate the two.

The loop (`loop_engine.rs`) runs `observe → decide → enact → commit`. A `Goal`
carries `id` + `revision`; between *decide* and *enact* the loop re-checks
supersession. If a newer revision arrived while the provider was deciding, the
in-flight decision is **discarded unexecuted** and the loop re-decides on the new
revision (the "abandon-path"). This is the streaming/voice mechanism and is the same
machinery a future hardware safety veto will reuse — so the veto is a question of
*who sets the flag*, not new plumbing.

Other core modules: `events.rs` (off-thread ordered event stream), `registry.rs`
(capability registry + lifecycle; a conflict is an error, never last-wins),
`plugind.rs` (wasm plugin manager, `~/.pan/plugins/`, SIGHUP reload — forward-looking,
not exercised by the daemon yet), `config.rs` (TOML at `~/.pan/config.toml` with
imports, `${VAR}` expansion, and `PAN_`-prefixed overrides).

## Daemon architecture (pan-daemon)

- **`wire.rs`** — envelope + body serde types. *The wire IS the contract*; every shape
  mirrors the JSON Schema on the Godot/REACHLOCK side.
- **`session.rs`** — per-connection state machine: handshake → `register_capabilities`
  → `instantiate_soul` → `perceive` (steady state) → `release_soul` / `shutdown`.
  Owns the capability registry, the souls, and the pipeline. A soul's `mind` selects
  its provider; `rules` minds parse a `rules: [...]` array out of their opaque
  birth-state JSON.
- **`server.rs`** — TCP loopback listener + NDJSON framing, single-connection (a new
  connect drops the old one). The protocol forbids non-loopback binding.
- **`governor.rs`** — the daemon's `govern` stage: allow iff the host registered the
  capability. (The wire-level `unknown_capability` error is raised earlier, at the
  pipeline's `resolve`/`validate` stage.)
- **`llm.rs`** — the LLM mind, same `Provider` trait as rules. Targets local plain-HTTP
  OpenAI-compatible / Ollama-native servers via a tiny std-only HTTP/1.0 client (no
  TLS, no async, no new deps). **Disabled unless `PAN_LLM_BASE` is set** (e.g.
  `http://127.0.0.1:11434`); `PAN_LLM_MODEL` optionally pins the model. The endpoint
  is probed once at startup; if unreachable, the daemon simply doesn't advertise the
  `llm` mind and llm-minded souls fall back to a Continue-only decision — the game
  must always run without a model. `perceive` runs on per-perceive worker threads,
  with supersession enforced at the enact boundary.

### Conformance — cross-repo contract

`pan-daemon/tests/fixtures/*.json` are **byte-identical** to
`reachlock/godot/framework/protocol/fixtures/*.json` and are the Godot side's golden
truth, checked in so this crate is self-contained for CI. `conformance.rs` round-trips
every fixture, asserts each body variant matches its envelope `type`, and asserts
every message type has ≥1 fixture. **If a fixture fails to deserialize, the contract
is broken — fix Pan, do NOT edit the fixture.** CI additionally runs REACHLOCK's
`soul-protocol-harness` (a separate `chezgoulet/reachlock` checkout) against the real
`pan serve` binary — a pan change that breaks the wire contract fails there.

## Notes

- Root-level `*.md`, `lib.rs`, and `*.py` files are historical design/synthesis
  artifacts and scaffolding, not the live codebase. The authoritative design docs are
  under `pan-core/docs/`.
- `pan-core` forbids `unsafe_code` (crate-level lint).
