//! The `pan` binary — unified CLI for the Pan agent platform.
//!
//! Subcommands:
//!
//! - `pan serve [--port N]`           — Soul Protocol daemon
//! - `pan run <Agent.toml>`           — interactive CLI agent
//! - `pan gateway [--port N] [--agents-dir DIR] [--auth-token T]` — HTTP server + web UI
//! - `pan tui <Agent.toml>`           — terminal agent UI
//! - `pan check-conformance`          — validate bundled protocol fixtures
//! - `pan --version` / `pan --help`   — version / usage

use std::path::PathBuf;
use std::process::ExitCode;

use pan_daemon::conformance::check_fixtures;
use pan_daemon::server::serve_loopback;

const DEFAULT_PORT: u16 = 40707;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_usage();
        return ExitCode::from(2);
    }
    match args[1].as_str() {
        "--help" | "-h" => {
            print_usage();
            ExitCode::SUCCESS
        }
        "--version" | "-V" => {
            println!("pan {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "serve" => run_serve(&args[2..]).await,
        "run" => run_agent(&args[2..]).await,
        "gateway" => run_gateway_cmd(&args[2..]).await,
        "tui" => run_tui_cmd(&args[2..]).await,
        "check-conformance" => run_check_conformance(),
        other => {
            eprintln!("pan: unknown subcommand `{other}`");
            print_usage();
            ExitCode::from(2)
        }
    }
}

fn print_usage() {
    eprintln!("pan — the Pan agent platform");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    pan serve [--port N]        Soul Protocol daemon");
    eprintln!("    pan run <Agent.toml>         Interactive CLI agent");
    eprintln!("    pan gateway [--port N]       HTTP server + web UI");
    eprintln!("                [--agents-dir DIR]");
    eprintln!("                [--auth-token T]");
    eprintln!("    pan tui <Agent.toml>         Terminal agent UI");
    eprintln!("    pan tui --code <Agent.toml>    Code agent UI (plan/build modes)");
    eprintln!("    pan check-conformance        Validate protocol fixtures");
    eprintln!("    pan --version / --help");
}

fn read_str<'a>(args: &'a [String], i: &mut usize) -> Option<&'a str> {
    let val = args.get(*i)?;
    *i += 1;
    Some(val.as_str())
}

// ---------------------------------------------------------------------------
// serve
// ---------------------------------------------------------------------------

async fn run_serve(args: &[String]) -> ExitCode {
    let mut port: Option<u16> = None;
    let mut i = 0;
    while i < args.len() {
        match args.get(i).map(String::as_str) {
            Some("--port") => {
                i += 1;
                port = read_str(args, &mut i).and_then(|s| s.parse().ok());
            }
            Some(_) => {
                eprintln!("pan serve: unknown argument `{}`", args[i]);
                return ExitCode::from(2);
            }
            None => break,
        }
    }
    let port = port
        .or_else(|| {
            std::env::var("REACHLOCK_PAN_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(DEFAULT_PORT);
    eprintln!("pan serve: binding 127.0.0.1:{port}");
    if let Err(e) = serve_loopback(port).await {
        eprintln!("pan serve: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

async fn run_agent(args: &[String]) -> ExitCode {
    let path = match args.first() {
        Some(p) => p,
        None => {
            eprintln!("pan run: missing <Agent.toml> path");
            return ExitCode::from(2);
        }
    };
    let manifest = match pan_agent::manifest::AgentManifest::load(path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("pan run: {e}");
            return ExitCode::FAILURE;
        }
    };
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let config_path = PathBuf::from(home).join(".pan").join("config.toml");
    let global_config = pan_core::config::Config::load(&config_path).ok();
    let agent = match pan_agent::assemble_with_config(
        &manifest,
        &pan_agent::builtin::builtin_registry(),
        global_config.as_ref(),
    ) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("pan run: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "pan run: `{}` (persona {}, provider {}) — type a line, /quit to exit.",
        agent.name,
        agent.scope.origin,
        agent.provider.id()
    );
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    match pan_cli::run_session(&agent, stdin, &mut stdout).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pan run: I/O error: {e}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// gateway
// ---------------------------------------------------------------------------

async fn run_gateway_cmd(args: &[String]) -> ExitCode {
    let mut port: Option<u16> = None;
    let mut agents_dir: Option<PathBuf> = None;
    let mut auth_token: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args.get(i).map(String::as_str) {
            Some("--port") => {
                i += 1;
                port = read_str(args, &mut i).and_then(|s| s.parse().ok());
            }
            Some("--agents-dir") => {
                i += 1;
                agents_dir = read_str(args, &mut i).map(PathBuf::from);
            }
            Some("--auth-token") => {
                i += 1;
                auth_token = read_str(args, &mut i).map(String::from);
            }
            Some(_) => {
                eprintln!("pan gateway: unknown argument `{}`", args[i]);
                return ExitCode::from(2);
            }
            None => break,
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
    if let Err(e) = pan_gateway::run_gateway(&agents_dir, port, auth_token.as_deref()).await {
        eprintln!("pan gateway: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// tui
// ---------------------------------------------------------------------------

async fn run_tui_cmd(args: &[String]) -> ExitCode {
    let path = match args.first() {
        Some(p) => p,
        None => {
            eprintln!("pan tui: missing <Agent.toml> path");
            return ExitCode::from(2);
        }
    };
    let manifest = match pan_agent::manifest::AgentManifest::load(path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("pan tui: {e}");
            return ExitCode::FAILURE;
        }
    };
    let registry = pan_agent::builtin::builtin_registry();
    let is_code = args.iter().any(|a| a == "--code");
    let mut agent = match pan_agent::assemble(&manifest, &registry) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("pan tui: {e}");
            return ExitCode::FAILURE;
        }
    };
    let plan_governor = if is_code {
        Some(pan_core::pipeline::ScopedGovernor::new())
    } else {
        None
    };
    if let Err(e) = pan_tui::run_tui(&mut agent, plan_governor).await {
        eprintln!("pan tui: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// check-conformance
// ---------------------------------------------------------------------------

fn run_check_conformance() -> ExitCode {
    let dir = fixtures_dir();
    eprintln!("checking conformance against {}", dir.display());
    let report = match check_fixtures(&dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };
    if report.is_ok() {
        println!(
            "ok   {} fixture(s), all {} message types covered",
            report.fixture_count, report.type_count
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("FAILED ({} problem(s)):", report.errors.len());
        for e in &report.errors {
            eprintln!("  - {e}");
        }
        ExitCode::from(1)
    }
}

fn fixtures_dir() -> PathBuf {
    let exe = std::env::current_exe().ok();
    if let Some(exe) = exe {
        let mut p = exe.as_path();
        loop {
            let candidate = p.join("pan-daemon/tests/fixtures");
            if candidate.is_dir() {
                return candidate;
            }
            match p.parent() {
                Some(parent) => p = parent,
                None => break,
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for ancestor in cwd.ancestors() {
        let candidate = ancestor.join("pan-daemon/tests/fixtures");
        if candidate.is_dir() {
            return candidate;
        }
    }
    cwd.join("tests/fixtures")
}
