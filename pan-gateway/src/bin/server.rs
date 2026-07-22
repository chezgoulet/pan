//! # `pan-gateway` — the Pan agentic AI gateway binary.
//!
//! ```sh
//! pan-gateway --agents-dir ./agents --port 8080 --auth-token <token>
//! ```
//!
//! Environment variables:
//! - `PAN_GATEWAY_PORT` — port (default 40707)
//! - `PAN_GATEWAY_AGENTS` — path to agents directory (default `./agents`)
//! - `PAN_GATEWAY_AUTH_TOKEN` — bearer token for auth (optional)

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use pan_core::config::Config;
use pan_gateway::handlers::{self, GatewayState};
use pan_gateway::AgentPool;

#[derive(Debug)]
struct Args {
    port: u16,
    agents_dir: PathBuf,
    auth_token: Option<String>,
}

fn parse_args() -> Args {
    let mut port: Option<u16> = None;
    let mut agents_dir: Option<PathBuf> = None;
    let mut auth_token: Option<String> = None;

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        std::process::exit(0);
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("pan-gateway {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }

    let mut raw_args = args.iter();
    while let Some(arg) = raw_args.next() {
        match arg.as_str() {
            "--port" => {
                port = raw_args.next().and_then(|s| s.parse().ok());
            }
            "--agents-dir" => {
                agents_dir = raw_args.next().map(PathBuf::from);
            }
            "--auth-token" => {
                auth_token = raw_args.next().map(String::from);
            }
            _ => {
                eprintln!("unknown flag: {arg}");
                print_usage();
                std::process::exit(1);
            }
        }
    }

    let port = port
        .or_else(|| {
            std::env::var("PAN_GATEWAY_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(40707);

    let agents_dir = agents_dir
        .or_else(|| std::env::var("PAN_GATEWAY_AGENTS").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("./agents"));

    let auth_token = auth_token
        .or_else(|| std::env::var("PAN_GATEWAY_AUTH_TOKEN").ok())
        .filter(|s| !s.is_empty());

    Args {
        port,
        agents_dir,
        auth_token,
    }
}

fn print_usage() {
    eprintln!(
        "Usage: pan-gateway [--port <PORT>] [--agents-dir <DIR>] [--auth-token <TOKEN>]\n\
         \n\
         Environment variables:\n\
           PAN_GATEWAY_PORT       port (default 40707)\n\
         PAN_GATEWAY_AGENTS      path to agents directory (default ./agents)\n\
         PAN_GATEWAY_AUTH_TOKEN  bearer token for auth (optional)"
    );
}

#[tokio::main]
async fn main() {
    let args = parse_args();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Load global config for default plugin/agents paths.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let config_path = PathBuf::from(home).join(".pan").join("config.toml");
    let global_config = Config::load(&config_path).ok();
    if global_config.is_some() {
        tracing::info!("loaded global config from {config_path:?}");
    }

    tracing::info!("loading agents from {:?}", args.agents_dir);
    let pool = match AgentPool::load_with_config(&args.agents_dir, global_config.as_ref()) {
        Ok(p) => {
            let names: Vec<&str> = p.names().collect();
            tracing::info!("loaded {} agent(s): {:?}", names.len(), names);
            p
        }
        Err(e) => {
            tracing::error!("failed to load agents: {e}");
            std::process::exit(1);
        }
    };

    let state = Arc::new(GatewayState::new(pool));
    let app = handlers::router(state);

    // Optional auth middleware
    let app = if let Some(token) = &args.auth_token {
        let token = token.clone();
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
                    if auth != format!("Bearer {expected}") {
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

    let addr = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 0], args.port));
    tracing::info!("pan-gateway listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("failed to bind {addr}: {e}");
            std::process::exit(1);
        });

    axum::serve(listener, app).await.unwrap_or_else(|e| {
        tracing::error!("server error: {e}");
        std::process::exit(1);
    });
}
