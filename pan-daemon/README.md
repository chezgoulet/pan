# pan-daemon — the Soul Protocol server

The mind daemon. Speaks the Soul Protocol over TCP loopback NDJSON, hosts
souls, decides what to do (rules provider in M1), and ships the decision
back to the host. The wire vocabulary lives in [`src/wire.rs`]; the
session state machine in [`src/session.rs`]; the TCP / NDJSON framing in
[`src/server.rs`]; the conformance fixtures in `tests/fixtures/`.

## Quick start

```sh
cargo build
cargo test                  # 19 unit + 19 conformance = 38 tests
./target/debug/pan check-conformance
./target/debug/pan serve --port 40707
```

The port can also be set via the `REACHLOCK_PAN_PORT` env var. The daemon
binds loopback only; the protocol forbids non-loopback binding.

## Architecture

- **`src/wire.rs`** — envelope + body serde types. The wire IS the
  contract: every shape mirrors the JSON Schema in
  `reachlock/godot/framework/protocol/schemas/soul_message.schema.json`.
- **`src/session.rs`** — per-connection state machine. Owns the registered
  capability set, the instantiated souls, the rules provider. Handles the
  hello / welcome / register / instantiate / perceive / decision / shutdown
  lifecycle.
- **`src/server.rs`** — TCP loopback listener, NDJSON framing, single-
  connection lifecycle (a new connect drops the old one).
- **`src/governor.rs`** — the daemon's `govern` stage. At M1, allow iff
  the host registered the capability; deny otherwise. (The wire-level
  `unknown_capability` check happens at the pipeline's `resolve` stage and
  is surfaced via `error: unknown_capability` wire messages.)
- **`src/conformance.rs`** — fixture loader + schema validator used by
  `pan check-conformance` and the `tests/conformance.rs` integration test.

## Conformance

The 15 fixtures under `tests/fixtures/` are the Godot side's golden
truth. They are byte-identical to
`reachlock/godot/framework/protocol/fixtures/*.json` — checked in to make
this crate self-contained for CI. The conformance suite:

- round-trips every fixture through Pan's wire types;
- asserts the body variant matches the envelope's `type` field;
- asserts every one of the 10 message types has ≥ 1 fixture;
- asserts each fixture survives compact NDJSON re-serialization.

If a fixture fails to deserialize, the contract is broken — fix Pan, do
NOT edit the fixture.

## M1 scope

- Server: `pan serve` over TCP loopback, NDJSON, single-connection.
- Provider: `rules` (event-topic + signal-threshold rules parsed from
  each soul's birth-state `rules: [...]` array). Non-rules minds are
  accepted and produce a Continue-only decision.
- Errors: `bad_frame`, `unknown_type`, `version_unsupported`,
  `unknown_soul`, `unknown_capability`, `invalid_args`,
  `provider_failure`. The wire's closed set, used in the canonical way.

Out of scope: LLM provider, BYOK, behavior-tree provider, persistent
souls, governance policy, observability beyond the in-memory event sink
the pipeline uses internally. These are Wave 2+ work.
