# Changelog

## 0.1.0 (unreleased)

### Added
- Core: async pipeline with type-state governance, origin-aware `ScopedGovernor`
- Core: ReAct loop — agentic tool-use with `MAX_TOOL_STEPS` bound, backward compatible
- Core: cancellable abandon-path (supersession drops in-flight `decide` futures)
- Core: `PipelineInvoker::sub()` for delegation with narrowed scope
- Core: host-allowlist `HostAllowlistGovernor` for `cap.http.*`
- Core: `VetoSource` trait for hardware safety veto on the abandon-path
- Core: `StreamingObservations` for voice/streaming goal input
- Core: streaming `token_tx` channel in `Loop` for per-intent SSE output
- Daemon: full async server (tokio `TcpListener`, `tokio::spawn` per perceive)
- Daemon: `ResolveGovernor` owns `Arc<CapabilityRegistry>` (no lifetime conflicts)
- Daemon: `SessionPipeline` struct for registry-built components
- Agent: `Agent.toml` manifest + assembler → scoped, governed agent
- Agent: global config merge (`~/.pan/config.toml` + `Agent.toml`)
- CLI: interactive REPL with cross-span conversation history
- CLI: governed capability execution (shell, state, fs)
- LLM: OpenAI-compatible function-calling provider
- LLM: Anthropic native Messages API provider
- LLM: TLS transport (rustls), retry with exponential backoff
- LLM: large-tool-output truncation, token budget tracking
- Capabilities: `cap.fs` (read/write/list/glob/search), `cap.shell`, `cap.state`
- Capabilities: `cap.http` (GET/POST), `cap.time` (now/today), `cap.skill` lifecycle
- Capabilities: `cap.agent.delegate` for multi-agent orchestration
- Skills: Python subprocess bridge with `ScopedInvoker` governance
- Skills: bwrap OS sandbox with namespace isolation
- Gateway: axum HTTP server with OpenAI-compatible API
- Gateway: per-intent SSE streaming through `token_tx` channel
- Gateway: agent delegation with scope narrowing
- Observability: `TracingSink` + `FnSink` event sinks, property tests
- CI: `.github/workflows/ci.yml` with fmt + clippy + test + verify.sh
- Packaging: release profile (LTO, single codegen, strip)

### Changed
- Everything — initial release
