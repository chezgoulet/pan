# Pan — agent harness

Pan is an agent harness in Rust. One core, driven by different plugin sets, powers a
chat assistant, game-NPC brains, and headless trend detection. **The central design
decision: the reasoning model is a plugin.** The core contains no prompt, no token
format, and no tool-call convention.

## Quick start

```sh
export PATH="$HOME/.cargo/bin:$PATH"

# Build everything
cargo build --release

# Run tests
cargo test --workspace

# Interactive agent
./target/release/pan run examples/agents/echo.toml

# HTTP gateway (open http://localhost:40707)
./target/release/pan gateway --agents-dir examples/agents

# Terminal UI
./target/release/pan tui examples/agents/echo.toml

# Soul Protocol daemon
./target/release/pan serve --port 40707
```

## Crates

| Crate | Role |
|---|---|
| `pan-core` | Core vocabulary, async pipeline, governance, ReAct loop |
| `pan-daemon` | Soul Protocol game-NPC daemon (`pan serve`) |
| `pan-agent` | `Agent.toml` manifest + assembler |
| `pan-cap` | Capability components: fs, shell, http, state, time, skill |
| `pan-cli` | Interactive CLI (`pan-agent run`) |
| `pan-llm` | LLM providers: OpenAI-compatible + Anthropic, TLS |
| `pan-skill` | Python skill runtime + bwrap sandbox |
| `pan-gateway` | OpenAI-compatible HTTP gateway with SSE streaming |

## Documentation

- [`docs/HANDOFF.md`](docs/HANDOFF.md) — session continuity, crate map, gotchas
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — sprint plan, what's left to build
- [`docs/INSTALL.md`](docs/INSTALL.md) — installation guide
- [`docs/CHANGELOG.md`](docs/CHANGELOG.md) — user-facing changes

## License

MIT OR Apache-2.0
