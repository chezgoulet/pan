//! # OS-level sandbox for skills (Linux namespaces via bubblewrap).
//!
//! [`sandbox_args`] returns the `bwrap` arguments that confine a skill process:
//! no network, read-only system directories, a tmpfs work area. This is the
//! "honest-scope gap" fix: skills can no longer open arbitrary sockets or files
//! outside their jail, even at the OS level.
//!
//! Bubblewrap (`bwrap`) must be installed and setuid (or the binary must have
//! `CAP_SYS_ADMIN` / user namespaces enabled). If `bwrap` is not found,
//! [`sandbox_args`] returns an error — skills degrade to the unsandboxed path.
//!
//! The sandbox profile is conservative: read-only `/usr`, `/lib`, `/lib64`,
//! `/bin`, `/etc`, plus `/proc` and `/dev`. Everything else is a tmpfs. Network
//! is denied via `--unshare-net`.

use std::path::Path;

/// Check if `bwrap` is available on `$PATH`.
pub fn bwrap_available() -> bool {
    which_bwrap().is_ok()
}

/// Build the `bwrap` argument list for sandboxing a skill.
///
/// The returned args should be passed to [`SkillRunner::with_program`]:
/// ```ignore
/// let runner = SkillRunner::with_program(
///     "bwrap",
///     sandbox_args(lib_dir, skill_path)?,
///     lib_dir,
/// )?;
/// ```
pub fn sandbox_args(lib_dir: &Path, skill_path: &Path) -> Result<Vec<String>, String> {
    if !bwrap_available() {
        return Err("bwrap not found on PATH".into());
    }

    let mut args: Vec<String> = Vec::new();

    // Bind-mount essential system directories read-only.
    for dir in &["/usr", "/lib", "/lib64", "/bin", "/etc"] {
        if Path::new(dir).exists() {
            args.push("--ro-bind".into());
            args.push(dir.to_string());
            args.push(dir.to_string());
        }
    }

    // Standard virtual filesystems.
    args.push("--proc".into());
    args.push("/proc".into());
    args.push("--dev".into());
    args.push("/dev".into());

    // Everything else is an empty tmpfs.
    for dir in &["/tmp", "/home", "/var", "/root", "/run"] {
        if Path::new(dir).exists() {
            args.push("--tmpfs".into());
            args.push(dir.to_string());
        }
    }

    // The skill's files: make the skill script and the pan.py lib available.
    if let Some(parent) = skill_path.parent() {
        args.push("--ro-bind".into());
        args.push(parent.to_string_lossy().into_owned());
        args.push(parent.to_string_lossy().into_owned());
    }
    if lib_dir.exists() {
        args.push("--ro-bind".into());
        args.push(lib_dir.to_string_lossy().into_owned());
        args.push(lib_dir.to_string_lossy().into_owned());
    }

    // Network isolation.
    args.push("--unshare-net".into());
    args.push("--unshare-uts".into());
    args.push("--unshare-ipc".into());

    // Drop all capabilities.
    args.push("--cap-drop".into());
    args.push("ALL".into());

    // The command to run inside the sandbox.
    args.push("python3".into());
    args.push(skill_path.to_string_lossy().into_owned());

    Ok(args)
}

fn which_bwrap() -> Result<(), String> {
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        let candidate = format!("{dir}/bwrap");
        if std::fs::metadata(&candidate).is_ok() {
            return Ok(());
        }
    }
    Err("not found".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_args_produces_valid_bwrap_invocation() {
        let lib = Path::new("/tmp/pan-lib");
        let skill = Path::new("/tmp/pan-skills/test.py");
        let result = sandbox_args(lib, skill);
        // bwrap may not be available in CI, but the arg structure should be sound.
        if let Ok(args) = result {
            assert!(args.contains(&"--ro-bind".to_string()));
            assert!(args.contains(&"--unshare-net".to_string()));
            assert!(args.contains(&"python3".to_string()));
            assert!(args.contains(&"/tmp/pan-skills/test.py".to_string()));
        }
    }

    #[test]
    #[serial_test::serial]
    fn bwrap_not_available_gracefully_degrades() {
        // Temporarily remove bwrap from PATH, if it's there.
        let old_path = std::env::var("PATH").ok();
        std::env::set_var("PATH", "/dev/null");
        assert!(!bwrap_available());
        if let Some(p) = old_path {
            std::env::set_var("PATH", p);
        }
    }
}
