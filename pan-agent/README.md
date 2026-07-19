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
# ...any other [persona] keys are passed to the provider factory (e.g. a rules array)

[caps]
enable = ["cap.state", "cap.fs"]  # which capability components exist (the toolbox)

[caps.grant]                    # deny-by-default; each true family grants cap.<family>
fs = true
state = true

[caps.settings."cap.fs"]        # per-component config
root = "/var/lib/pan/agent-root"
```

A **persona** is one concept: the capabilities the agent may invoke, the voice it
follows, and the provider that drives it. Assembling it yields an `AssembledAgent`
carrying *everything a loop needs*:

- a `Scope` (`persona.assistant`) — the authority every effect is stamped with;
- a `ScopedGovernor` built from `[caps.grant]` — deny-by-default;
- the `Provider` named by `persona.provider`, built through the `ComponentRegistry`;
- a `Toolbox` of the `[caps.enable]` components — the pipeline's capability registry
  (`toolbox.registry()`) *and* its executor (`&toolbox`).

An unknown provider or capability id, or `cap.fs` without a `root`, is a load-time
error — not a late surprise.

## The payoff

One `Agent.toml` becomes a running, governed agent. The capstone test assembles a
manifest (a rules brain + enabled, rooted `cap.fs` + an `fs` grant), drives one
loop span, and a **real file appears on disk** — config to running agent, no
hand-wiring.

```rust
use pan_agent::{assemble_toml, builtin_registry};
use pan_core::pipeline::Pipeline;

let agent = assemble_toml(agent_toml, &builtin_registry())?;
let registry = agent.toolbox.registry();
let pipeline = Pipeline {
    registry: &registry,
    governor: &agent.governor,
    executor: &agent.toolbox,   // the toolbox IS the executor
    events: &stream,
};
// ...drive a Loop with agent.provider under agent.scope.
```

`builtin_registry()` is the component set a stock binary ships with (the pan-core
providers plus pan-cap's `cap.state` / `cap.fs`); a deployment registers its own
components on top.

## Run it

```sh
cargo test -p pan-agent
```
