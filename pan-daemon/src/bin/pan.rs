//! The `pan` binary.
//!
//! Subcommands:
//!
//! - `pan serve [--port N]`  — run the Soul Protocol daemon on `127.0.0.1:N`.
//!   The port can also be set via the `REACHLOCK_PAN_PORT` env var. If
//!   neither is set, the daemon uses the canonical default 40707.
//! - `pan check-conformance` — run the conformance suite against the bundled
//!   fixtures and exit 0 on pass, 1 on fail. Intended for CI; the same
//!   fixture set runs in `cargo test`'s `tests/conformance.rs`.
//! - `pan --version` / `pan --help` — print the version / usage and exit.

use std::path::PathBuf;
use std::process::ExitCode;

use pan_daemon::conformance::check_fixtures;
use pan_daemon::server::serve_loopback;

/// Canonical default port when neither `--port` nor `REACHLOCK_PAN_PORT` is set.
const DEFAULT_PORT: u16 = 40707;

fn main() -> ExitCode {
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
        "serve" => run_serve(&args[2..]),
        "check-conformance" => run_check_conformance(),
        other => {
            eprintln!("unknown subcommand: {other}");
            print_usage();
            ExitCode::from(2)
        }
    }
}

fn print_usage() {
    eprintln!("pan — the mind daemon (Soul Protocol server)");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    pan serve [--port N]    Bind 127.0.0.1:N and run the daemon.");
    eprintln!("                            N defaults to $REACHLOCK_PAN_PORT or 40707.");
    eprintln!("    pan check-conformance   Run the conformance suite against bundled fixtures.");
    eprintln!("    pan --version           Print the version and exit.");
    eprintln!("    pan --help              Print this message.");
}

fn run_serve(args: &[String]) -> ExitCode {
    let mut port: Option<u16> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--port" {
            i += 1;
            let v = match args.get(i) {
                Some(s) => s,
                None => { eprintln!("--port requires a value"); return ExitCode::from(2); }
            };
            port = Some(match v.parse::<u16>() {
                Ok(p) => p,
                Err(e) => { eprintln!("invalid --port: {e}"); return ExitCode::from(2); }
            });
        } else if let Some(rest) = a.strip_prefix("--port=") {
            port = Some(match rest.parse::<u16>() {
                Ok(p) => p,
                Err(e) => { eprintln!("invalid --port: {e}"); return ExitCode::from(2); }
            });
        } else {
            eprintln!("unknown argument: {a}");
            return ExitCode::from(2);
        }
        i += 1;
    }
    // Resolution order: --port, then $REACHLOCK_PAN_PORT, then DEFAULT_PORT.
    let port = port
        .or_else(|| std::env::var("REACHLOCK_PAN_PORT").ok().and_then(|s| s.parse().ok()))
        .unwrap_or(DEFAULT_PORT);

    eprintln!("pan serve: binding 127.0.0.1:{port}");
    if let Err(e) = serve_loopback(port) {
        eprintln!("server error: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn run_check_conformance() -> ExitCode {
    let dir = fixtures_dir();
    eprintln!("checking conformance against {}", dir.display());
    let report = match check_fixtures(&dir) {
        Ok(r) => r,
        Err(e) => { eprintln!("error: {e}"); return ExitCode::from(1); }
    };
    if report.is_ok() {
        println!("ok   {} fixture(s), all {} message types covered",
            report.fixture_count, report.type_count);
        ExitCode::SUCCESS
    } else {
        eprintln!("FAILED ({} problem(s)):", report.errors.len());
        for e in &report.errors { eprintln!("  - {e}"); }
        ExitCode::from(1)
    }
}

/// Locate the `tests/fixtures/` directory. The binary's CWD may vary; we
/// walk up from the executable to find the workspace root by looking for
/// `Cargo.toml` with our workspace marker.
fn fixtures_dir() -> PathBuf {
    // The fixtures are bundled at `pan-daemon/tests/fixtures/`. In dev /
    // CI, that's at `target/debug/pan` + `/../../../pan-daemon/tests/fixtures`.
    // We try a few likely candidates.
    let exe = std::env::current_exe().ok();
    if let Some(exe) = exe {
        // Walk up to find a `Cargo.toml` declaring the workspace.
        let mut p = exe.as_path();
        loop {
            let candidate = p.join("pan-daemon/tests/fixtures");
            if candidate.is_dir() { return candidate; }
            match p.parent() {
                Some(parent) => p = parent,
                None => break,
            }
        }
    }
    // Fallback: try the workspace root relative to CWD.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for ancestor in cwd.ancestors() {
        let candidate = ancestor.join("pan-daemon/tests/fixtures");
        if candidate.is_dir() { return candidate; }
    }
    // Last resort: the `tests/fixtures` next to the binary.
    cwd.join("tests/fixtures")
}
