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

# Help
./target/release/pan --help

# Interactive CLI agent
./target/release/pan run examples/agents/echo.toml

# Terminal UI (streaming tokens, /undo, code mode)
./target/release/pan tui examples/agents/echo.toml

# HTTP gateway (open http://localhost:40707)
./target/release/pan gateway --agents-dir examples/agents

# Soul Protocol daemon
./target/release/pan serve --port 40707
```

## Crates (9)

| Crate | Role |
|---|---|
| `pan-core` | Core vocabulary, async pipeline, governance, ReAct loop, compactor, evaluator, hooks |
| `pan-daemon` | Soul Protocol game-NPC daemon (`pan serve`) |
| `pan-agent` | `Agent.toml` manifest + assembler, session store, context assemblers |
| `pan-cap` | Capability components: fs, shell, http, state, time, skill, format, lsp |
| `pan-cli` | Interactive CLI (`pan run`) |
| `pan-llm` | LLM providers: OpenAI-compatible + Anthropic, TLS, compactor, evaluator |
| `pan-skill` | Python skill runtime + bwrap sandbox |
| `pan-gateway` | OpenAI-compatible HTTP gateway with SSE streaming + web UI |
| `pan-tui` | Terminal agent UI with streaming, code mode, slash commands |

## Documentation

- [`docs/USER-GUIDE.md`](docs/USER-GUIDE.md) — comprehensive user guide (13 sections)
- [`docs/HANDOFF.md`](docs/HANDOFF.md) — session continuity, crate map, gotchas
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — sprint plan, what's built
- [`docs/INSTALL.md`](docs/INSTALL.md) — installation guide
- [`docs/CHANGELOG.md`](docs/CHANGELOG.md) — user-facing changes
- [`docs/decisions/`](docs/decisions/) — architecture decision records

## License

MIT OR Apache-2.0
