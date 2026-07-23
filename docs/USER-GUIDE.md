# Pan User Guide

Everything you need to install, configure, run, and extend Pan — the agent
harness where the reasoning model is a plugin.

---

## 1. Installation

### Quick install

```sh
git clone <repo-url>
cd pan
cargo build --release            # ~5 min first build
export PATH="$HOME/.cargo/bin:$PATH"

# Verify
./target/release/pan --version
./target/release/pan --help
```

`pan` is the unified binary. Move it into your PATH:

```sh
cp target/release/pan ~/.local/bin/   # or /usr/local/bin
```

### Requirements

- **Rust** 1.75+
- **Python skills** (optional): `python3` on PATH
- **Skill sandbox** (optional): `bwrap` on PATH (Linux only)
- **LLM providers** (optional): a running Ollama, llama.cpp, OpenAI-compatible
  endpoint, or Anthropic API key

### Environment

`cargo` is a rustup shim — not always on PATH:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

You can put this in `~/.bashrc` or prefix every command.

---

## 2. Five-minute quick start

### 2.1 Run the echo agent

```sh
cat > /tmp/echo.toml << 'EOF'
[meta]
name = "echo"
persona = "assistant"
[persona]
instruction = "You are an echo."
provider = "provider.echo"
[caps.grant]
shell = true
state = true
EOF

pan tui /tmp/echo.toml
```

Type anything and press Enter. The agent echoes it back. Press **Esc** to quit.

### 2.2 Run the command agent

```sh
cat > /tmp/doer.toml << 'EOF'
[meta]
name = "doer"
persona = "assistant"
[persona]
provider = "provider.command"
[caps]
enable = ["cap.shell", "cap.state", "cap.fs"]
[caps.grant]
shell = true
state = true
fs = true
[caps.settings."cap.fs"]
root = "/tmp/pan-root"
EOF

mkdir -p /tmp/pan-root
pan tui /tmp/doer.toml
```

Now type commands like `run echo hello`, `remember pet=cat`, `recall pet`.

### 2.3 Run with a real LLM

You need a running model server. With
[Ollama](https://ollama.com):

```sh
ollama pull llama3.2
```

Then run:

```toml
# /tmp/llm.toml
[meta]
name = "helper"
persona = "assistant"
[persona]
provider = "provider.llm"
instruction = "You are a helpful assistant."
base = "http://127.0.0.1:11434/v1"
model = "llama3.2"
max_tokens = 1024
temperature = 0.7
[caps]
enable = ["cap.shell", "cap.state", "cap.fs", "cap.http"]
[caps.grant]
shell = true
state = true
fs = true
http = true
[caps.settings."cap.fs"]
root = "/tmp/pan-root"
```

```sh
pan tui /tmp/llm.toml
```

The LLM can now call tools (shell, filesystem, HTTP, state) through the
governed pipeline.

---

## 3. Concepts

### 3.1 Architecture

Pan's central design: **the reasoning model is a plugin.** The core contains
no prompt, no token format, and no tool-call convention. Every brain
(echo, command, LLM, behavior tree, rules engine) implements the same
`Provider` trait and emits the same `ActionIntent` vocabulary:

- `Invoke { capability, args }` — call a governed capability
- `Express { body }` — emit response text
- `Conclude { outcome }` — signal goal complete

The pipeline enforces **non-bypassable governance**:

```
EffectRequest --resolve--> --validate--> --govern--> --execute--> Effected
                                               ^
                                    (only Allow yields this)
```

An `Invoke` cannot skip governance. The `Governed` token has a private
constructor — only `govern()` can produce one.

### 3.2 Agent lifecycle

1. **Agent.toml** — a config file: what brain, what capabilities, what grants
2. **Assemble** — the manifest is parsed and components are built from the
   `ComponentRegistry`
3. **AssembledAgent** — carries `{ scope, governor, provider, toolbox }`
4. **Pipeline + Loop** — runs one span per goal (observation → decide →
   enact → commit)
5. **ReAct** — a provider that `Invoke`s without `Conclude` gets the result
   fed back and re-decides on the same goal. Bounded by `MAX_TOOL_STEPS`.

### 3.3 What is a capability?

A capability is a named effect (`cap.shell.run`, `cap.fs.write`) that is:

- **Declared** by a `CapabilityProvider` component
- **Enabled** by listing it in `[caps.enable]`
- **Governed** by `[caps.grant]` — the persona's origin must have a matching
  prefix grant
- **Arg-checked** at `validate` stage (required fields, types)

---

## 4. Agent.toml reference

### 4.1 Structure

```toml
[meta]
name = "agent-name"            # Required. Used in scope origin.
persona = "assistant"          # Required. Origin suffix.

[persona]
provider = "provider.llm"      # Required. Which brain.
instruction = "You are..."     # System prompt / role description.
base = "http://..."            # LLM base URL (provider-specific).
model = "llama3.2"             # LLM model name (provider-specific).
api_key = "sk-..."             # API key (provider-specific, avoid in files).
max_tokens = 1024              # Max tokens per response (LLM).
temperature = 0.7              # Temperature (LLM).
token_budget = 100000          # Optional cumulative token cap.
context = "context.rolling_history"  # Optional context assembler.

# Extra settings passed through to the provider's factory.
# (rules arrays, prefix strings, etc.)
setting_name = "value"

[caps]
enable = ["cap.shell", "cap.state"]   # Which capability components to load.

[caps.grant]
shell = true        # Grants cap.shell.* to the persona's origin.
state = true        # Grants cap.state.*
fs = true           # Grants cap.fs.*
http = true         # Grants cap.http.*
format = true       # Grants cap.format.*
lsp = true          # Grants cap.lsp.*
skill = true        # Grants cap.skill.*
agent = true        # Grants cap.agent.*

[caps.settings."cap.fs"]
root = "/tmp/pan-root"                # Required for cap.fs
snapshot_root = "~/.pan/snapshots"    # Optional: enables undo

[caps.settings."cap.state"]
path = "memory.json"                  # Optional: persistence file

[caps.settings."cap.skill"]
root = "/tmp/pan-skills"              # Required for cap.skill

[caps.settings."cap.http"]
# No required settings (host allowlisting is done via governor)
```

### 4.2 Context assemblers

| Setting value | Effect |
|---------------|--------|
| `context = "context.rolling_history"` | Last N turns in memory (set `max_turns`) |
| `context = "context.memory_retrieval"` | Reads cap.state, injects matching facts |
| `context = "context.session"` | JSONL-backed persistent history (set `path`) |

Example with session persistence:

```toml
[persona]
context = "context.session"

[persona.settings]
# Session file path (required for context.session)
path = "~/.pan/sessions/helper.jsonl"
max_turns = 200
```

### 4.3 Per-capability settings

Each capability component reads its own settings from `[caps.settings."cap.<name>"]`:

| Capability | Setting | Required | Description |
|---|---|---|---|
| `cap.fs` | `root` | Yes | Jail directory for all file operations |
| `cap.fs` | `snapshot_root` | No | Enables auto-snapshots + `/undo` |
| `cap.state` | `path` | No | File path for persistent state (JSON) |
| `cap.skill` | `root` | Yes | Directory for skill files |
| `cap.skill` | `lib_dir` | No | Python library path (default: /tmp/pan-skill-lib) |
| `cap.state` | *(none)* | — | In-memory KV if no path given |
| `cap.shell` | *(none)* | — | No settings needed |
| `cap.http` | *(none)* | — | Host policy lives in the governor |
| `cap.time` | *(none)* | — | No settings needed |
| `cap.format` | *(none)* | — | No settings needed |
| `cap.lsp` | *(none)* | — | No settings needed |

---

## 5. Subcommands — running Pan

### 5.1 `pan tui` — terminal UI

The flagship channel. Full-screen terminal with:

- Real-time streaming token display (tokens appear as the LLM generates them)
- Split-pane: conversation (70%) + activity/tool panel (30%)
- Markdown rendering (`**bold**`, `*italic*`, `` `code` ``)
- Plan/Build mode toggle (Tab key)
- Code mode: `pan tui --code agent.toml` starts in Plan mode

**Keyboard shortcuts:**

| Key | Action |
|-----|--------|
| `Enter` | Submit input |
| `/undo <path>` | Restore latest snapshot for path |
| `/undo list <path>` | List snapshots for a path |
| `/help` | Show all slash commands |
| `/clear` or `Ctrl+L` | Clear conversation |
| `/quit` or `/exit` | Quit |
| `Ctrl+C` | Cancel running span |
| `Tab` | Toggle Plan/Build mode |
| `Esc` | Quit |
| `Up` / `Down` | Input history |
| `PageUp` / `PageDown` | Scroll conversation |

```sh
# Normal mode
pan tui my-agent.toml

# Code mode (starts with stripped-down Plan governor, Tab enables full Build)
pan tui --code my-agent.toml
```

### 5.2 `pan run` — CLI REPL

Line-based interactive agent:

```sh
pan run my-agent.toml
```

Commands: type input and press Enter. The agent's response is printed.
`/quit` or `/exit` exits. No streaming (responses appear when complete).

### 5.3 `pan gateway` — HTTP server

OpenAI-compatible API, web UI, SSE streaming:

```sh
# Serve agents from a directory
pan gateway --agents-dir ./examples/agents --port 40707

# With auth
pan gateway --agents-dir ./examples/agents --auth-token "mytoken"
```

Open `http://localhost:40707` for the web UI.

**API endpoints:**

| Endpoint | Method | Description |
|---|---|---|
| `/v1/chat/completions` | POST | OpenAI-compatible chat (supports `stream: true`) |
| `/v1/agents` | GET | List available agents |
| `/v1/agents/:name/goals` | POST | Pan-native goal dispatch |
| `/health` | GET | Health check |

**curl examples:**

```sh
# Chat
curl http://localhost:40707/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"helper","messages":[{"role":"user","content":"Hello"}]}'

# Streaming
curl --no-buffer http://localhost:40707/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"helper","messages":[{"role":"user","content":"Tell me a story"}],"stream":true}'

# Agent goal
curl http://localhost:40707/v1/agents/helper/goals \
  -H 'Content-Type: application/json' \
  -d '{"objective":"What is the weather?"}'
```

### 5.4 `pan serve` — Soul Protocol daemon

For game NPC integration. Speaks the Soul Protocol over TCP loopback:

```sh
pan serve --port 40707
```

Soul Protocol is a JSON-line protocol over TCP. The daemon:

- Handshake → register capabilities → instantiate soul → perceive
  (steady state) → release soul
- Each soul has a `mind` that selects its provider
- Conformance-verified against fixtures (run `pan check-conformance`)

### 5.5 `pan check-conformance`

Validates that the daemon's wire protocol matches the cross-repo Soul
Protocol fixtures:

```sh
pan check-conformance
```

Exit code 0 = all 19 fixtures pass. Used in CI.

---

## 6. Capabilities reference

### `cap.shell`

Run a program directly (no shell — no injection class).

```json
{
  "cap.shell.run": { "program": "echo", "args": ["hello", "world"] }
}
```

Returns `{ "stdout": "...", "stderr": "...", "exit_code": 0 }`.

### `cap.state`

In-memory key-value store. Optional file persistence.

```json
{ "cap.state.set": { "key": "name", "value": "Alice" } }
{ "cap.state.get": { "key": "name" } }
```

Returns `{ "value": "Alice" }` or `{ "value": null }`.

### `cap.fs`

Rooted filesystem access (jailed under `root`). Paths are relative, `..`
is refused, absolute paths are refused.

```json
{ "cap.fs.read":   { "path": "file.txt" } }
{ "cap.fs.write":  { "path": "file.txt", "content": "text" } }
{ "cap.fs.list":   { "path": "." } }
{ "cap.fs.glob":   { "pattern": "**/*.rs" } }
{ "cap.fs.search": { "path": ".", "query": "TODO" } }
{ "cap.fs.undo":   { "path": "file.txt" } }
{ "cap.fs.undo":   { "path": "file.txt", "snapshot_id": "12345" } }
{ "cap.fs.undo":   { "path": "file.txt", "_list": true } }
```

With `snapshot_root` set, every `write` auto-snapshots the existing file.
`undo` restores the latest (or a specific) snapshot. `_list` returns
available snapshots.

### `cap.http`

HTTP GET and POST requests. Host allowlisting is done by the governor
(`HostAllowlistGovernor`), not baked into the capability.

```json
{ "cap.http.get":  { "url": "https://api.example.com/data" } }
{ "cap.http.post": { "url": "https://api.example.com/submit", "body": "..." } }
```

Returns `{ "status": 200, "body": "..." }`.

### `cap.time`

Current date and time. Models love to hallucinate dates — use this.

```json
{ "cap.time.now":  {} }   // "2026-07-23T15:30:00Z"
{ "cap.time.today": {} }  // "2026-07-23"
```

### `cap.skill`

Create, edit, list, delete, and run Python skills. Skills run as governed
subprocesses with a `ScopedInvoker`.

```json
{ "cap.skill.create": { "name": "math", "source": "def run(args): return 2+2" } }
{ "cap.skill.run":    { "name": "math", "args": {} } }
{ "cap.skill.list":   {} }
{ "cap.skill.edit":   { "name": "math", "source": "..." } }
{ "cap.skill.delete": { "name": "math" } }
```

### `cap.format`

Auto-format files by extension. Dispatches to the appropriate formatter:

| Extension | Formatter |
|-----------|-----------|
| `.rs` | `rustfmt` |
| `.js`, `.ts`, `.jsx`, `.tsx` | `npx prettier --write` |
| `.json`, `.jsonc`, `.md`, `.css`, `.scss`, `.less`, `.yaml`, `.yml` | `npx prettier --write` |
| `.py` | `ruff format` |
| `.go` | `gofmt` |
| `.toml` | `taplo format` |

```json
{ "cap.format.run": { "path": "src/main.rs" } }
```

### `cap.lsp`

Language diagnostics and format checking. Per-extension checkers:

| Extension | Checker | Format checker |
|-----------|---------|----------------|
| `.rs` | `rustc --edition 2021 --crate-type lib` | `rustfmt --check` |
| `.py` | `ruff check --output-format=json` | `ruff format --check` |
| `.ts`, `.tsx` | `npx tsc --noEmit` | `npx prettier --check` |
| `.js`, `.jsx`, `.mjs` | `node --check` | `npx prettier --check` |
| `.go` | `go vet` | `gofmt -l` |

```json
{ "cap.lsp.check":  { "path": "src/main.rs" } }
{ "cap.lsp.format": { "path": "src/main.rs" } }
```

### `cap.agent.delegate`

Multi-agent orchestration. Delegates a goal to a sub-agent:

```json
{ "cap.agent.delegate": { "agent": "helper", "objective": "Calculate 2+2" } }
```

---

## 7. Providers reference

### `provider.echo`

Echoes input back. Useful for testing and debugging. No model needed.

Settings:
- `prefix` (optional) — prepended to echoed output (e.g. `"🤖 Echo: "`)

### `provider.command`

Deterministic interpreter. Maps utterances to capabilities:

| Input | Action |
|-------|--------|
| `run <program> <args>` | `cap.shell.run` |
| `remember <key>=<value>` | `cap.state.set` |
| `recall <key>` | `cap.state.get` |
| `write <path> <content>` | `cap.fs.write` |

No model needed. Only the capabilities you enabled and granted are available.

### `provider.llm`

OpenAI-compatible function-calling LLM. Works with:

- Ollama (`http://127.0.0.1:11434/v1`)
- llama.cpp (`http://127.0.0.1:8080/v1`)
- LM Studio (`http://127.0.0.1:1234/v1`)
- OpenAI (`https://api.openai.com/v1`)
- OpenRouter (`https://openrouter.ai/api/v1`)
- Groq (`https://api.groq.com/openai/v1`)
- Together (`https://api.together.xyz/v1`)
- Any OpenAI-compatible endpoint

Settings:
| Setting | Source | Required |
|---------|--------|----------|
| `base` | `[persona]` or `PAN_LLM_BASE` env | Yes |
| `model` | `[persona]` or `PAN_LLM_MODEL` env | Yes |
| `api_key` | `[persona]` or `PAN_LLM_API_KEY` env | For cloud endpoints |
| `instruction` | `[persona]` | System prompt |
| `max_tokens` | `[persona]` | Default 512 |
| `temperature` | `[persona]` | Default 0.7 |
| `token_budget` | `[persona]` | Cumulative token cap |

Features:
- Maps capabilities to OpenAI function tools
- Replays tool results for multi-step ReAct reasoning
- Truncates large tool outputs (>32K chars)
- Retries with exponential backoff on 429/5xx
- TLS via rustls (no cmake/system certs needed)

### `provider.anthropic`

Anthropic native Messages API (`/v1/messages`). Uses `x-api-key` auth.

Settings:
| Setting | Source | Required |
|---------|--------|----------|
| `base` | `[persona]` or `PAN_LLM_BASE` env | Yes |
| `model` | `[persona]` or `PAN_LLM_MODEL` env | Yes |
| `api_key` | `[persona]` or `PAN_ANTHROPIC_API_KEY` env | Yes |
| `instruction` | `[persona]` | System prompt |
| `max_tokens` | `[persona]` | Default 1024 |
| `token_budget` | `[persona]` | Cumulative token cap |

### `provider.rules`

Game NPC brain. Interprets a set of rules from the persona settings:

```toml
[persona]
provider = "provider.rules"

[persona.settings]
rules = [
  { when: "greeted", then: ["express(Hello!)", "conclude(achieved)"] },
  { when: "asked_about_weather", then: ["invoke(cap.time.now, {})"] }
]
```

### `provider.behaviortree`

Behavior tree NPC brain. Reads a tree definition from persona settings.

---

## 8. Governance

### 8.1 Grant model

`[caps.grant]` maps origin → allowed capability prefixes. Each entry grants
all capabilities whose id starts with the prefix:

```toml
[caps.grant]
shell = true    # grants cap.shell.* (cap.shell.run, etc.)
state = true    # grants cap.state.*
fs = true       # grants cap.fs.* (cap.fs.read, cap.fs.write, cap.fs.undo, etc.)
```

Deny-by-default: an origin with no grant entry is denied everything. A
granted prefix matches the capability via prefix + dotted suffix:
`"cap.fs"` matches `"cap.fs.read"` but not `"cap.fsx"`.

### 8.2 Path-scoped rules

`PathGovernor` adds file-path-level rules for `cap.fs.*` capabilities.
Rules are checked in order; the first match wins (allow or deny).

```rust
use pan_core::pipeline::{ScopedGovernor, PathGovernor, PolicyChain};

let governor = PolicyChain::new()
    .push(Box::new(ScopedGovernor::new().grant("persona.assistant", vec!["cap.fs"])))
    .push(Box::new(
        PathGovernor::new(Box::new(AllowAll))
            .allow_path("cap.fs", "/home/**")
            .deny_path("cap.fs", "/etc/**")
    ));
```

The `pan tui --code` mode uses this pattern: a stripped-down governor for
Plan mode (read-only) and a full governor for Build mode.

### 8.3 Host allowlisting

`HostAllowlistGovernor` wraps an inner governor and adds URL-level controls
for `cap.http.*`:

```rust
use pan_core::pipeline::HostAllowlistGovernor;

let gov = HostAllowlistGovernor::new(Box::new(inner_gov))
    .allow_hosts("persona.assistant", vec!["*.example.com", "api.trusted.org"]);
```

### 8.4 Policy chain

`PolicyChain` composes multiple governors. The first non-`Allow` verdict
wins (fail-fast):

```
ScopedGovernor(arrive → deny/allow)
  → PathGovernor(arrive → deny/pass)
    → HostAllowlistGovernor(arrive → deny/pass)
```

---

## 9. Advanced features

### 9.1 SnapshotStore — `/undo`

With `snapshot_root` configured in `[caps.settings."cap.fs"]`, every
`cap.fs.write` auto-snapshots the existing file before overwriting.

**In TUI:**
```
/undo src/main.rs           # restore latest snapshot
/undo list src/main.rs      # list available snapshots
/undo src/main.rs 12345     # restore specific snapshot
```

**From the agent:**
The LLM can call `cap.fs.undo { path }` to restore a file when it detects
it made a mistake.

Snapshots are stored at `{snapshot_root}/{escaped_path}/{timestamp}.snap`.

### 9.2 SessionStore — persistent history

With `context = "context.session"` and a `path` setting, conversation
history is persisted to a JSONL file. Restart Pan and the agent remembers
previous turns:

```toml
[persona]
context = "context.session"

[persona.settings]
path = "~/.pan/sessions/helper.jsonl"
max_turns = 200
```

Each turn stores: goal, expressed text, and tool results.

### 9.3 Context budget + compactor

When the working context grows large (many tool results), the
`TruncationCompactor` drops the oldest non-essential fragments until the
estimated token count fits within budget:

```toml
[persona]
context_budget = 4096    # max tokens for working context
```

The compactor preserves system/objective/persona fragments and drops
oldest tool_result/history/memory fragments first.

### 9.4 Goal evaluator

After a span concludes with `Achieved`, an optional `GoalEvaluator` can
check whether the goal was actually satisfied. If not, the span ends with
`Unsatisfied`:

```toml
[persona]
evaluator = "evaluator.llm"
```

The `LlmEvaluator` uses a lightweight model (default `llama3.2:1b`) for a
fast yes/no check. Configure via persona settings:

```toml
[persona.settings]
evaluator_base = "http://127.0.0.1:11434/v1"
evaluator_model = "llama3.2:1b"
```

### 9.5 Lifecycle hooks

Every effect execution fires hooks registered on the Pipeline. Built-in:
`LoggingHook` writes each effect to stderr:

```
[hook] persona.assistant/logger cap.fs.write args={"path":"file.txt","content":"..."}
[hook] persona.assistant/logger cap.fs.write ok={"bytes":42}
```

### 9.6 Wasm plugins

Pan supports out-of-process Wasm plugins (distinct from in-process
components). Plugins are `.wasm` files in `~/.pan/plugins/` with a
`plugin.toml` manifest:

```toml
name = "my-plugin"
version = "1.0.0"
[capabilities]
provides = ["cap.custom.*"]
needs = ["cap.state.*"]
```

The daemon auto-discovers plugins on startup and SIGHUP reloads them.

---

## 10. Configuration

### 10.1 Global config (`~/.pan/config.toml`)

Global settings serve as defaults that per-agent `Agent.toml` settings
override:

```toml
[persona]
base = "http://127.0.0.1:11434/v1"
model = "llama3.2"
max_tokens = 1024

[persona.settings]
token_budget = 50000

[caps.settings."cap.fs"]
root = "/var/lib/pan/agent-root"
snapshot_root = "~/.pan/snapshots"
```

### 10.2 Environment variables

| Variable | Overrides | Used by |
|----------|-----------|---------|
| `PAN_LLM_BASE` | `[persona] base` | `provider.llm`, `provider.anthropic` |
| `PAN_LLM_MODEL` | `[persona] model` | `provider.llm`, `provider.anthropic` |
| `PAN_LLM_API_KEY` | `[persona] api_key` | `provider.llm` |
| `PAN_ANTHROPIC_API_KEY` | `[persona] api_key` | `provider.anthropic` |
| `PAN_LLM_TIMEOUT` | — | HTTP request timeout (default 60s) |

### 10.3 Variable expansion

Config values containing `${VAR_NAME}` are expanded from environment
variables:

```toml
[persona]
api_key = "${PAN_LLM_API_KEY}"
```

---

## 11. Deployment examples

### 11.1 Production gateway with auth

```sh
pan gateway \
  --port 443 \
  --agents-dir /etc/pan/agents \
  --auth-token "$(cat /etc/pan/auth-token)"
```

### 11.2 Systemd service

```ini
# /etc/systemd/system/pan-gateway.service
[Unit]
Description=Pan agent gateway
After=network.target

[Service]
ExecStart=/usr/local/bin/pan gateway \
  --port 40707 \
  --agents-dir /etc/pan/agents \
  --auth-token $(cat /etc/pan/auth-token)
Environment=PAN_LLM_BASE=http://127.0.0.1:11434/v1
Restart=always
User=pan

[Install]
WantedBy=multi-user.target
```

### 11.3 Game NPC daemon

```sh
pan serve --port 40707
```

The game (Godot/REACHLOCK) connects from the same machine. Only loopback
connections are accepted.

---

## 12. Troubleshooting

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `unknown provider` | Typo or unregistered component | Check provider name; `provider.llm` vs `provider.echo` |
| `cap.fs requires a root setting` | Missing `root` in settings | Add `[caps.settings."cap.fs"] root = "/path"` |
| `snapshot store not configured` | `/undo` called without `snapshot_root` | Add `snapshot_root` to cap.fs settings |
| Agent doesn't remember past turns | No context assembler | Add `context = "context.rolling_history"` |
| `token budget exhausted` | Cumulative token cap reached | Increase `token_budget` or remove it |
| `can't connect to model` | LLM server not running | Start Ollama/llama.cpp; check `base` URL |
| `capability denied` | Missing grant | Add `shell = true` etc. to `[caps.grant]` |
| TUI not showing output | Terminal not ANSI-capable | Use `pan run` instead |
| Gateway 404 | Agent.toml not in --agents-dir | Check agent name matches filename |
| Compile-fail guard breaks | Toolchain regression | Run `pan-core/verify.sh` to diagnose |

---

## 13. Quick reference card

```
pan tui agent.toml           # Terminal UI (streaming)
pan run agent.toml           # CLI REPL
pan gateway --agents-dir .   # HTTP server (port 40707)
pan serve --port 40707       # Game NPC daemon
pan check-conformance        # Protocol verification

# TUI shortcuts: /help, /undo <path>, Tab toggle, Ctrl+C cancel
# /undo list <path> for snapshots
```

**One-shot agent run:**
```sh
echo "run echo hello" | pan run agent.toml
```

**Config structure:**
```toml
[meta] name, persona
[persona] provider, instruction, base, model, context, settings.*
[caps] enable = [...]
[caps.grant] shell/state/fs/http/format/lsp/skill/agent = true
[caps.settings."cap.<name>"] key = value
```
