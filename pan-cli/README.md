# pan-cli — run an `Agent.toml` as an interactive agent

The thin layer that turns an assembled agent into a running REPL. Each input line
becomes a `Goal` (an `Utterance`); one loop span decides + enacts it under the
agent's scope, governor, and toolbox; the provider's `Express` output is written
back. This is the plan's `channel.cli` — and it is *thin* precisely because
`pan-agent`'s `assemble` already produced the whole graph.

## Use it

```sh
cargo run -p pan-cli --bin pan-agent -- run Agent.toml
```

```text
$ printf 'hello pan\n/quit\n' | pan-agent run Agent.toml
pan-agent: `demo` (persona persona.assistant, provider provider.echo) — type a line, /quit to exit.
echo: hello pan
```

With this `Agent.toml`:

```toml
[meta]
name = "demo"
persona = "assistant"

[persona]
provider = "provider.echo"   # answers out of the box
prefix = "echo"              # provider-specific setting from config
```

## Provider-agnostic by design

The CLI is just another origin of goals; the intelligence is the configured
provider:

- **`provider.echo`** — replies to each line (dependency-free; good for trying the
  loop and for tests).
- **`provider.command`** — a typed command interpreter: `run <program> [args…]`,
  `remember <key> <value…>`, `recall <key>`, `write <path> <content…>` map to
  `cap.shell` / `cap.state` / `cap.fs` invokes. Makes the CLI actually *do* things.
- **`provider.rules` / `provider.behaviortree`** — react to events/signals.
- **A real LLM provider** (behind the same `Provider` trait) — makes it
  conversational. The harness doesn't change; only the brain does.

Every effect a provider decides on still flows through the governed pipeline —
the CLI grants no special authority. Effect results (a shell's stdout, a recalled
value) are shown via `RunReport.results`; an ungranted capability is denied at
`govern` and reported as an error.

```text
$ printf 'run echo hi\nremember pet cat\nrecall pet\n/quit\n' | pan-agent run doer.toml
$ echo hi
hi
remembered `pet`
recalling `pet`:
= "cat"
```

## Library

`run_session(agent, reader, writer)` drives the REPL over any async byte streams;
`run_session_on_bytes(agent, input)` is the in-memory form the tests use. The
binary (`pan-agent`) is a thin `main` over `run_session` on real stdin/stdout.

## Run it

```sh
cargo test -p pan-cli
```
