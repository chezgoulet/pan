# Pan — agent harness

Pan is an agent harness in Rust. One core, driven by different plugin sets, powers a
chat assistant, game-NPC brains, and headless trend detection. **The central design
decision: the reasoning model is a plugin.** The core contains no prompt, no token
format, and no tool-call convention — so an LLM, a behavior tree, and a rules engine
all ride the same contract.

**One unified binary.** `pan` is a single statically-linked binary with five
subcommands: CLI agent, terminal UI, HTTP gateway, Soul Protocol daemon, and
conformance validation. Everything — providers, capabilities, governance — is
assembled from an `Agent.toml` manifest file.

## Install

**One-liner** (Linux x86_64, places `pan` in `~/.local/bin`):

```sh
mkdir -p ~/.local/bin && curl -fsSL https://github.com/chezgoulet/pan/releases/latest/download/pan -o ~/.local/bin/pan && chmod +x ~/.local/bin/pan
```

Or download and run the install script:

```sh
curl -fsSL https://raw.githubusercontent.com/chezgoulet/pan/main/install.sh | sh
```

Make sure `~/.local/bin` is on your `PATH`:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

[Full install guide](docs/INSTALL.md) (includes build-from-source and cargo install).

## Quick start

```sh
pan --version
pan --help
```

```sh
# Interactive CLI agent
pan run examples/agents/echo.toml

# Terminal UI — streaming tokens, code mode, /undo
pan tui examples/agents/echo.toml

# HTTP gateway + web UI (open http://localhost:40707)
pan gateway --agents-dir examples/agents

# Soul Protocol daemon (game NPCs)
pan serve --port 40707
```

## What's included

| Subcommand | Description |
|---|---|
| `pan serve` | Soul Protocol daemon — TCP loopback NDJSON for game NPCs |
| `pan run <Agent.toml>` | Interactive CLI agent — type goals, see replies + capability results |
| `pan gateway` | OpenAI-compatible HTTP API, SSE streaming, built-in web UI |
| `pan tui <Agent.toml>` | Terminal UI with streaming token output, code mode, markdown, /undo |
| `pan check-conformance` | Validate bundled Soul Protocol wire fixtures |

**Key features:**
- **Agent.toml assembly** — persona, provider, capabilities, and grants in one file
- **Governance by construction** — type-state pipeline makes ungoverned effects impossible to express
- **ReAct loop** — tools produce results, the provider re-decides until it concludes (bounded)
- **Plugin model** — echo, command, rules, behavior tree, OpenAI, or Anthropic — same contract
- **Real capabilities** — filesystem (with undo), shell, HTTP, state (KV), time, Python skills (bwrap sandbox), LSP diagnostics, code formatting, multi-agent delegation
- **Streaming** — per-token output in TUI, per-intent SSE in the gateway

## Architecture

Pan is 9 Rust crates composing into a single binary:

| Crate | Role |
|---|---|
| `pan-core` | Vocabulary, async pipeline, governance, ReAct loop, hooks, evaluator |
| `pan-daemon` | Soul Protocol server, conformance fixtures, unified `pan` binary |
| `pan-agent` | `Agent.toml` manifest + assembler, session store, context assemblers |
| `pan-cap` | Capabilities: fs, shell, http, state, time, skill, format, lsp |
| `pan-cli` | Interactive REPL |
| `pan-llm` | LLM providers (OpenAI + Anthropic), TLS transport |
| `pan-skill` | Python skill runtime + bwrap OS sandbox |
| `pan-gateway` | HTTP gateway with SSE streaming + web UI |
| `pan-tui` | Terminal agent UI (ratatui/crossterm), streaming, code mode |

**Through-line:** `Agent.toml → assemble → { Scope, Governor, Provider, Toolbox } → Pipeline + Loop → governed capability runs.`

## Build from source

Requires Rust 1.75+.

```sh
git clone https://github.com/chezgoulet/pan.git
cd pan
cargo build --release
./target/release/pan --version
```

## Documentation

| Doc | Description |
|---|---|
| [User Guide](docs/USER-GUIDE.md) | Full reference — 13 sections covering config, providers, capabilities, TUI, gateway |
| [Install Guide](docs/INSTALL.md) | Installation methods, requirements, verification |
| [Changelog](docs/CHANGELOG.md) | User-facing changes per release |
| [Architecture Decisions](docs/decisions/) | ADRs for governance, scope, invoker, component wiring |

## License

MIT OR Apache-2.0
