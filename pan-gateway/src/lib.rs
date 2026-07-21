//! # pan-gateway — the Pan agentic AI gateway.
//!
//! An HTTP server that exposes [`AssembledAgent`]s over a Streamable HTTP API,
//! compatible with the OpenAI `/v1/chat/completions` endpoint. Each request runs
//! through the full Pan pipeline (govern → execute → ReAct loop) and returns
//! the agent's response as JSON or a streaming data channel.
//!
//! ## Endpoints
//!
//! - `POST /v1/chat/completions` — OpenAI-compatible. The `model` field selects
//!   an agent from the pool; `messages` are converted to a `Goal` with history.
//!   Set `stream: true` for a Streamable HTTP response.
//! - `POST /v1/agents/:name/goals` — Pan-native. Pass a `Goal` body directly.
//! - `GET /health` — health check.
//!
//! ## Agent pool
//!
//! Agents are loaded from `Agent.toml` files in a directory (default
//! `./agents/`). Each file becomes a pool entry keyed by `meta.name`.

pub mod handlers;
pub mod pool;

pub use pool::AgentPool;
