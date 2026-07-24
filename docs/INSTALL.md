# Installing Pan

## From GitHub releases (Linux)

Pre-built binaries are published with each release on
[GitHub](https://github.com/chezgoulet/pan/releases).

**One-liner** (downloads to `~/.local/bin`):

```sh
mkdir -p ~/.local/bin && curl -fsSL https://github.com/chezgoulet/pan/releases/latest/download/pan -o ~/.local/bin/pan && chmod +x ~/.local/bin/pan
```

**Using the install script:**

```sh
curl -fsSL https://raw.githubusercontent.com/chezgoulet/pan/main/install.sh | sh
```

The script automatically picks the latest release and installs to `~/.local/bin`.
Set `PREFIX` to change the install location:

```sh
PREFIX=/usr/local curl -fsSL https://raw.githubusercontent.com/chezgoulet/pan/main/install.sh | sh
```

**Platform support:** Linux x86_64. For other platforms, build from source (below).

## From source

```sh
git clone https://github.com/chezgoulet/pan.git
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

| Method | Required |
|---|---|
| GitHub release | Linux x86_64, `curl` or `wget` |
| Build from source | Rust 1.75+, `cargo` |
| `cargo install` | Rust 1.75+, `cargo` |

**Optional runtime dependencies:**
- **Python skills**: `python3` on PATH
- **Skill sandbox**: `bwrap` on PATH (Linux only)
- **LLM providers**: Ollama, llama.cpp, OpenAI-compatible endpoint, or Anthropic API key

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
