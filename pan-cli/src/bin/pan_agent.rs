//! The `pan-agent` binary: run an `Agent.toml` as an interactive agent.
//!
//! ```text
//! pan-agent run <Agent.toml>   Assemble the manifest and start a stdin/stdout REPL.
//! pan-agent --help
//! ```

use std::process::ExitCode;

use pan_agent::{assemble, builtin_registry, AgentManifest};
use pan_cli::run_session;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("run") => match args.get(2) {
            Some(path) => run(path).await,
            None => {
                eprintln!("pan-agent run: missing <Agent.toml> path");
                ExitCode::from(2)
            }
        },
        Some("--help") | Some("-h") | None => {
            print_usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("pan-agent: unknown command `{other}`");
            print_usage();
            ExitCode::from(2)
        }
    }
}

async fn run(path: &str) -> ExitCode {
    let manifest = match AgentManifest::load(path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("pan-agent: {e}");
            return ExitCode::FAILURE;
        }
    };
    let agent = match assemble(&manifest, &builtin_registry()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("pan-agent: {e}");
            return ExitCode::FAILURE;
        }
    };

    eprintln!(
        "pan-agent: `{}` (persona {}, provider {}) — type a line, /quit to exit.",
        agent.name,
        agent.scope.origin,
        agent.provider.id()
    );

    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    match run_session(&agent, stdin, &mut stdout).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pan-agent: I/O error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!("pan-agent — run an Agent.toml as an interactive agent");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    pan-agent run <Agent.toml>   Assemble and start a stdin/stdout REPL.");
    eprintln!("    pan-agent --help");
}
