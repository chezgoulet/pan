# Installing Pan

## From source

```sh
git clone <repo-url>
cd pan
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release

# The unified binary:
#   target/release/pan
# Copy to PATH:
cp target/release/pan ~/.local/bin/
```

## Using `cargo install`

```sh
cargo install --path pan-daemon --bin pan
```

## Requirements

- **Rust**: 1.75+
- **Build**: `cargo`, standard Rust toolchain
- **Python skills** (optional): `python3` on PATH
- **Skill sandbox** (optional): `bwrap` on PATH (Linux only)
- **LLM providers** (optional): Ollama, llama.cpp, or OpenAI-compatible endpoint

## Subcommands

| Command | Description |
|---------|-------------|
| `pan serve --port N` | Soul Protocol daemon (game NPCs) |
| `pan run <Agent.toml>` | Interactive CLI agent |
| `pan gateway --agents-dir DIR` | HTTP server + web UI (port 40707) |
| `pan tui <Agent.toml>` | Terminal UI with streaming tokens |
| `pan tui --code <Agent.toml>` | Code agent UI (plan/build modes) |
| `pan check-conformance` | Validate Soul Protocol fixtures |

## Verifying

```sh
pan --version
pan --help

# Smoke test with the echo provider
cat > /tmp/test.toml << 'EOF'
[meta] name = "test"
[persona] provider = "provider.echo"
[caps.grant] shell = true
EOF
echo "hello" | pan run /tmp/test.toml
# Should print: hello
```

## Next steps

See [USER-GUIDE.md](USER-GUIDE.md) for full documentation, configuration
examples, and feature reference.
