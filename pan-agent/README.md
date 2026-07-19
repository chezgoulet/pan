# pan-agent — `Agent.toml` and the assembler

One file per agent instance is the source of truth for *which* components an
agent runs and *what authority* they carry. This crate parses that file
([`AgentManifest`]) and assembles it ([`assemble`]) into a scoped, wired
[`AssembledAgent`]. It is the config model the plan settles before plugins
proliferate (Design Decision #1: Agent.toml, not env vars), and the point where
the [ADR 0001](../docs/decisions/0001-scope-invoker-components.md) interfaces
(Scope, ComponentRegistry) become a running graph instead of hand-wired code.

## The manifest

```toml
[meta]
name = "pan-default"
persona = "assistant"          # also the governance origin: persona.assistant

[persona]
instruction = "You are a helpful agent running in a terminal."
provider = "provider.rules"    # a ComponentRegistry id
model = "claude-sonnet-4-6"    # optional, provider-specific

[caps.grant]                    # deny-by-default; each true family grants cap.<family>
shell = true
fs = false
http = true
memory = true
```

A **persona** is one concept: the capabilities the agent may invoke, the voice it
follows, and the provider that drives it. Assembling it yields:

- a `Scope` (`persona.assistant`) — the authority every effect is stamped with;
- a `ScopedGovernor` built from `[caps.grant]` — `shell`/`http`/`memory` granted,
  `fs` denied, everything else denied;
- the `Provider` named by `persona.provider`, built through the
  `ComponentRegistry` (an unknown id is a load-time error, not a late surprise).

## The payoff

Config becomes enforcement with no hand-wiring. From the tests: an agent
assembled from the manifest above dispatches `cap.shell.run` (allowed) and
`cap.fs.read` (denied at `govern`) purely because of what `[caps.grant]` said.

```rust
use pan_agent::{assemble_toml, builtin_registry};

let agent = assemble_toml(agent_toml, &builtin_registry())?;
// agent.scope, agent.governor, agent.provider — ready to wire into a Pipeline/Loop.
```

`builtin_registry()` is the component set a stock binary ships with (the pan-core
providers today); a deployment registers its own components on top.

## Run it

```sh
cargo test -p pan-agent
```
