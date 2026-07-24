//! The `pan` binary — unified CLI for the Pan agent platform.
//!
//! Subcommands:
//!
//! - `pan serve [--port N]`           — Soul Protocol daemon
//! - `pan run <Agent.toml>`           — interactive CLI agent
//! - `pan gateway [--port N] [--agents-dir DIR] [--auth-token T]` — HTTP server + web UI
//! - `pan tui <Agent.toml>`           — terminal agent UI
//! - `pan check-conformance`          — validate bundled protocol fixtures
//! - `pan update`                     — update to the latest GitHub release
//! - `pan --version` / `pan --help`   — version / usage

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use pan_daemon::conformance::check_fixtures;
use pan_daemon::server::serve_loopback;

const DEFAULT_PORT: u16 = 40707;
const UPDATE_TIMEOUT: Duration = Duration::from_secs(30);
const VERSION_CHECK_TIMEOUT: Duration = Duration::from_secs(3);
const GH_API_LATEST: &str = "https://api.github.com/repos/chezgoulet/pan/releases/latest";

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
        "serve" => {
            spawn_version_check();
            run_serve(&args[2..]).await
        }
        "run" => {
            spawn_version_check();
            run_agent(&args[2..]).await
        }
        "gateway" => {
            spawn_version_check();
            run_gateway_cmd(&args[2..]).await
        }
        "tui" => {
            spawn_version_check();
            run_tui_cmd(&args[2..]).await
        }
        "check-conformance" => run_check_conformance(),
        "update" => run_update().await,
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
    eprintln!("    pan update                    Update to latest GitHub release");
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
    let pos = args.iter().position(|a| a != "--code").unwrap_or(0);
    let path = match args.get(pos) {
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
    let agent = match pan_agent::assemble(&manifest, &registry) {
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
    if let Err(e) = pan_tui::run_tui(Arc::new(agent), plan_governor).await {
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

// ---------------------------------------------------------------------------
// version check (non-blocking)
// ---------------------------------------------------------------------------

/// Fire off a background check to see if a newer Pan release exists.
/// Runs once, prints to stderr if one is available, silently ignores errors.
fn spawn_version_check() {
    tokio::spawn(async {
        if let Ok(val) = pan_llm::http::get_json_async(GH_API_LATEST, VERSION_CHECK_TIMEOUT).await {
            if let Some(tag) = val.get("tag_name").and_then(|v| v.as_str()) {
                let latest = tag.trim_start_matches('v');
                if compare_versions(latest, env!("CARGO_PKG_VERSION")) > 0 {
                    eprintln!("pan: newer version available: {tag}");
                    eprintln!("pan: run 'pan update' to install it");
                }
            }
        }
    });
}

/// Compare two semver version strings. Returns positive if `a > b`,
/// negative if `a < b`, zero if equal.
fn compare_versions(a: &str, b: &str) -> i32 {
    fn parse(v: &str) -> Vec<u64> {
        v.split('.').filter_map(|s| s.parse().ok()).collect()
    }
    let va = parse(a);
    let vb = parse(b);
    for i in 0..va.len().max(vb.len()) {
        let na = va.get(i).copied().unwrap_or(0);
        let nb = vb.get(i).copied().unwrap_or(0);
        if na != nb {
            return if na > nb { 1 } else { -1 };
        }
    }
    0
}

// ---------------------------------------------------------------------------
// update
// ---------------------------------------------------------------------------

/// Download the latest release binary from GitHub and replace the current one.
async fn run_update() -> ExitCode {
    let current = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("pan update: cannot determine current binary path: {e}");
            return ExitCode::FAILURE;
        }
    };

    let parent = current.parent().unwrap();
    if !parent.is_dir() {
        eprintln!("pan update: cannot determine parent directory");
        return ExitCode::FAILURE;
    }

    // Quick writability check (fast-fail before download).
    let test_file = parent.join(".pan-update-test");
    if std::fs::write(&test_file, b"").is_err() {
        eprintln!("pan update: no write permission for {}", parent.display());
        eprintln!("  Try: sudo pan update");
        eprintln!("  Or download from: https://github.com/chezgoulet/pan/releases");
        return ExitCode::FAILURE;
    }
    let _ = std::fs::remove_file(&test_file);

    eprintln!("pan update: checking for latest release...");

    let json = match pan_llm::http::get_json_async(GH_API_LATEST, UPDATE_TIMEOUT).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pan update: cannot fetch latest release: {e}");
            return ExitCode::FAILURE;
        }
    };

    let tag = match json.get("tag_name").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => {
            eprintln!("pan update: unexpected GitHub API response");
            return ExitCode::FAILURE;
        }
    };

    let dl_url = format!("https://github.com/chezgoulet/pan/releases/download/{tag}/pan");
    eprintln!("pan update: downloading {tag}...");

    let bytes = match pan_llm::http::get_bytes_async(&dl_url, UPDATE_TIMEOUT).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("pan update: download failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    if bytes.len() < 1_000_000 {
        eprintln!(
            "pan update: downloaded file too small ({} bytes) — aborting",
            bytes.len()
        );
        return ExitCode::FAILURE;
    }

    let tmp = current.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp, &bytes) {
        eprintln!("pan update: write to temp file failed: {e}");
        let _ = std::fs::remove_file(&tmp);
        return ExitCode::FAILURE;
    }

    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| {
            eprintln!("pan update: chmod failed: {e}");
            let _ = std::fs::remove_file(&tmp);
        })
        .ok();

    if let Err(e) = std::fs::rename(&tmp, &current) {
        eprintln!("pan update: rename failed: {e}");
        let _ = std::fs::remove_file(&tmp);
        return ExitCode::FAILURE;
    }

    // Quick sanity: the new binary actually runs.
    let version_out = match std::process::Command::new(&current)
        .arg("--version")
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => "unknown version".to_string(),
    };

    eprintln!("pan update: updated to {tag} ({version_out})");
    eprintln!("pan update: restart to use the new version");
    ExitCode::SUCCESS
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
