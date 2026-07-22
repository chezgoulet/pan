use std::path::PathBuf;
use std::sync::Arc;

use pan_gateway::handlers::{self, GatewayState};
use pan_gateway::AgentPool;

pub fn temp_agent_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let agent_path = dir.path().join("echo.toml");
    let toml = r#"[meta]
name = "echo"
persona = "assistant"

[persona]
instruction = "You are an echo. Repeat what the user says."
provider = "provider.echo"
prefix = "Echo: "

[caps.grant]
state = true
"#;
    std::fs::write(&agent_path, toml).expect("write agent toml");
    let path = dir.path().to_path_buf();
    (dir, path)
}

pub fn setup_test_app() -> (axum::Router, Arc<GatewayState>, tempfile::TempDir) {
    let (_dir, agent_path) = temp_agent_dir();
    let pool = AgentPool::load(&agent_path).expect("load agent pool");
    let state = Arc::new(GatewayState::new(pool));
    let app = handlers::router(Arc::clone(&state));
    (app, state, _dir)
}
