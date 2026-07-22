# Installing Pan

## From source

```sh
git clone <repo-url>
cd pan
cargo build --release

# The unified binary:
#   target/release/pan

# Subcommands:
#   pan serve --port 40707       Soul Protocol daemon
#   pan run <Agent.toml>         Interactive agent
#   pan gateway --agents-dir DIR HTTP server + web UI
#   pan tui <Agent.toml>         Terminal agent UI
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

## Verifying

```sh
pan --version
pan --help
```
