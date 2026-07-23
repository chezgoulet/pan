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

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::handlers::GatewayState;
use pan_core::config::Config;

/// Run the gateway server. Extracted from the original `pan-gateway` binary
/// so it can be called from the unified `pan` binary.
pub async fn run_gateway(
    agents_dir: &std::path::Path,
    port: u16,
    auth_token: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let config_path = PathBuf::from(home).join(".pan").join("config.toml");
    let global_config = Config::load(&config_path).ok();
    if global_config.is_some() {
        tracing::info!("loaded global config from {config_path:?}");
    }

    tracing::info!("loading agents from {agents_dir:?}");
    let pool = match AgentPool::load_with_config(agents_dir, global_config.as_ref()) {
        Ok(p) => {
            let names: Vec<&str> = p.names().collect();
            tracing::info!("loaded {} agent(s): {:?}", names.len(), names);
            p
        }
        Err(e) => {
            tracing::error!("failed to load agents: {e}");
            return Err(e.into());
        }
    };

    let state = Arc::new(GatewayState::new(pool));
    let app = handlers::router(state);

    let app = if let Some(token) = auth_token {
        let token = token.to_string();
        tracing::info!("auth token configured");
        app.layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let expected = token.clone();
                async move {
                    let auth = req
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    let expected_bearer = format!("Bearer {expected}");
                    if auth.len() != expected_bearer.len()
                        || auth
                            .bytes()
                            .zip(expected_bearer.bytes())
                            .any(|(a, b)| a != b)
                    {
                        let mut resp = axum::response::Response::new(axum::body::Body::from(
                            r#"{"error":"unauthorized"}"#,
                        ));
                        *resp.status_mut() = axum::http::StatusCode::UNAUTHORIZED;
                        return resp;
                    }
                    next.run(req).await
                }
            },
        ))
    } else {
        app
    };

    let addr = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 0], port));
    tracing::info!("pan gateway listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
