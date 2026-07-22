# Installing Pan

## From source

```sh
git clone <repo-url>
cd pan
cargo build --release

# The binaries are at:
#   target/release/pan          (daemon)
#   target/release/pan-agent    (CLI)
#   target/release/pan-gateway  (HTTP gateway)
```

## Using `cargo install`

```sh
cargo install --path pan-daemon --bin pan
cargo install --path pan-cli --bin pan-agent
cargo install --path pan-gateway --bin pan-gateway
```

## Requirements

- **Rust**: 1.75+
- **Build**: `cargo`, standard Rust toolchain
- **Python skills** (optional): `python3` on PATH
- **Skill sandbox** (optional): `bwrap` on PATH (Linux only)

## Verifying

```sh
pan --version
pan-agent --version
pan-gateway --version
```
