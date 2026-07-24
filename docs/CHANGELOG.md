# Changelog

## 0.1.2 (2026-07-23)

### Fixed
- Reconstructed assistant tool-call messages no longer include `"content": null`
  — some providers (OpenCode Go, DeepSeek) reject null content when `tool_calls`
  are present. The `content` key is now omitted entirely, matching OpenAI spec.

### Tests
- New `assistant_tool_call_omits_null_content_in_serialized_json` regression test
- `transcript_reconstructs_the_tool_exchange` now asserts no `content` key

## 0.1.1 (2026-07-23)

### Added
- `pan update` subcommand — downloads and atomically replaces the current binary
  with the latest GitHub release
- Non-blocking version check on `pan serve`/`run`/`gateway`/`tui` startup —
  alerts when a newer release is available
- `get_bytes_async` / `get_json_async` HTTP GET helpers in `pan-llm::http`

### Fixed
- `pan tui --code <Agent.toml>` argument parsing — `--code` flag is no longer
  consumed as the Agent.toml path

### Docs
- Comprehensive README with quick install, subcommands, and architecture
- GitHub release install method in INSTALL.md
- `install.sh` — POSIX install script for Linux x86_64
- `examples/agents/pan.toml` — full-capability example agent

## 0.1.0 (2026-07-23)

### Added
- Core: async pipeline with type-state governance, origin-aware `ScopedGovernor`
- Core: ReAct loop — agentic tool-use with `MAX_TOOL_STEPS` bound, backward compatible
- Core: cancellable abandon-path (supersession drops in-flight `decide` futures)
- Core: `PipelineInvoker::sub()` for delegation with narrowed scope
- Core: host-allowlist `HostAllowlistGovernor` for `cap.http.*`
- Core: `VetoSource` trait for hardware safety veto on the abandon-path
- Core: `StreamingObservations` for voice/streaming goal input
- Core: streaming `token_tx` channel in `Loop` for per-intent SSE output
- Core: `ContextBudget` (token estimation) + `ContextCompactor` trait + `TruncationCompactor`
- Core: `GoalEvaluator` trait + `RunEnd::Unsatisfied` variant
- Core: `EffectHook` trait (`pre_invoke`/`post_invoke`) + `LoggingHook`
- Core: `PathGovernor` — file-path-level governance with glob rules
- Core: `PolicyChain` — compose multiple governors with fail-fast semantics
- Core: `ScopedGovernor` now `Clone` for Arc sharing
- Daemon: full async server (tokio `TcpListener`, `tokio::spawn` per perceive)
- Daemon: `ResolveGovernor` owns `Arc<CapabilityRegistry>` (no lifetime conflicts)
- Daemon: `SessionPipeline` struct for registry-built components
- Agent: `Agent.toml` manifest + assembler → scoped, governed agent
- Agent: global config merge (`~/.pan/config.toml` + `Agent.toml`)
- Agent: `ContextAssembler` trait with three implementations: rolling history,
  memory retrieval (reads cap.state), session (JSONL-persisted)
- Agent: `SessionStore` — JSONL-backed conversation persistence across restarts
- Agent: `SnapshotStore` — directory-based file snapshots for undo
- CLI: interactive REPL with cross-span conversation history
- CLI: governed capability execution (shell, state, fs)
- TUI: ratatui terminal app with streaming tokens, plan/build mode toggle,
  markdown rendering, tool activity panel, input history, keyboard shortcuts
- TUI: real-time streaming (tokens appear as generated via `tokio::select!`)
- TUI: slash-command system (`/undo`, `/undo list`, `/help`, `/clear`, `/quit`)
- TUI: code mode (`--code`) with Plan/Build governor switching
- LLM: OpenAI-compatible function-calling provider
- LLM: Anthropic native Messages API provider
- LLM: TLS transport (rustls), retry with exponential backoff
- LLM: large-tool-output truncation, token budget tracking
- LLM: `LlmEvaluator` — lightweight goal satisfaction check
- Capabilities: `cap.fs` (read/write/list/glob/search/undo), `cap.shell`, `cap.state`
- Capabilities: `cap.http` (GET/POST), `cap.time` (now/today), `cap.skill` lifecycle
- Capabilities: `cap.agent.delegate` for multi-agent orchestration
- Capabilities: `cap.format` — auto-format files by extension (rustfmt, prettier, ruff)
- Capabilities: `cap.lsp` — language diagnostics (rustc, ruff, tsc, node --check, go vet)
- Skills: Python subprocess bridge with `ScopedInvoker` governance
- Skills: bwrap OS sandbox with namespace isolation
- Gateway: axum HTTP server with OpenAI-compatible API
- Gateway: per-intent SSE streaming through `token_tx` channel
- Gateway: agent delegation with scope narrowing
- Gateway: static HTML/JS web frontend
- Wasm: plugin system with wasmtime instantiation, C-ABI exports, PluginSet swap,
  PluginManager file-watch discovery + SIGHUP reload
- Observability: `TracingSink` + `FnSink` event sinks, property tests
- CI: `.github/workflows/ci.yml` with fmt + clippy + test + verify.sh
- Packaging: release profile (LTO, single codegen, strip)
- Docs: comprehensive USER-GUIDE.md (13 sections), updated INSTALL.md and CHANGELOG.md

### Changed
- Everything — initial release
- All deferred roadmap items landed (SnapshotStore, SessionStore, Compactor,
  GoalEvaluator, cap.lsp, hooks/path rules/policy chain)
- One unified `pan` binary with subcommands (serve, run, gateway, tui, check-conformance)
- TUI streaming from post-hoc drain to real-time via tokio::spawn
