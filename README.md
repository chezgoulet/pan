# Pan — the mind daemon and its core

Pan is an agent harness in Rust. The same core, with different plugin
sets, drives a chat assistant, game-NPC brains, and headless trend
detection. **The reasoning model is a plugin** — the core contains no
prompt, no token format, no tool-call convention. That single decision
is what lets a behavior tree or a rules engine stand in for an LLM
without pretending to be one.

Sprint 01's Pan workstream delivers the **daemon**: a server that
speaks the Soul Protocol over TCP loopback NDJSON, hosts souls, decides
what to do, and ships the decision back to the host.

## Workspace

```
pan/
├── pan-core/         The irreducible core: vocabulary, dispatch pipeline,
│                     loop, event stream, plugin lifecycle. (Wave 0; settled.)
├── pan-daemon/       The Soul Protocol server. `pan serve`, fixtures, tests.
└── Cargo.toml        Workspace manifest.
```

## Quick start

```sh
cargo build
cargo test                                  # 68 tests pass
./target/debug/pan check-conformance        # 15 fixtures, 10 message types OK
./target/debug/pan serve --port 40707       # or REACHLOCK_PAN_PORT
```

## Conformance

Pan and the Godot host share the same wire contract. The 15 fixtures in
`pan-daemon/tests/fixtures/` are byte-identical to
`reachlock/godot/framework/protocol/fixtures/*.json` and validated by
both sides in their own CI. The Python check lives at
`reachlock/scripts/check_soul_protocol.py`; the Rust check is
`pan-daemon/tests/conformance.rs`.

## Sprint 01 (this) — what landed

- **P1**: `pan serve` daemon. TCP loopback, NDJSON, single-connection.
  Hello / welcome / register_capabilities / instantiate_soul /
  release_soul / perceive / decision / shutdown lifecycle.
- **P2**: Rules provider end-to-end through validate → govern → enact.
  A `Trigger::Event { topic, .. }` matching a soul's `when_event_topic`
  rule fires the rule's `then_invoke` (an `ActionIntent::Invoke`).
- **P4**: Capability registry from the host's `register_capabilities`.
  The provider's `decide` only chooses among registered capabilities;
  `validate` rejects `Invoke` of an unregistered one with
  `error code: "unknown_capability"` (conformance fixture 09).
- **P5**: 15-fixture conformance suite in Pan's CI. All 10 message
  types covered.

Out of scope (deferred):

- LLM provider (P3) — the rules provider is enough for M1.
- BYOK and local llama.cpp.
- REACHLOCK client (the Godot side owns this).
