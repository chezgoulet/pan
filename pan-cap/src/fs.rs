//! # `cap.fs` — rooted filesystem access.
//!
//! Provides `cap.fs.read`, `cap.fs.write`, and `cap.fs.list`, all confined to a
//! root directory. The governor decides *whether* a persona may reach `cap.fs`
//! at all; this component adds **defense in depth** at the executor: every path
//! is jailed under the root, and absolute paths or `..` traversal are refused
//! outright. So even a persona granted `cap.fs` cannot read outside its root.
//!
//! File I/O is blocking `std::fs` inside the async method — fine for the small
//! reads/writes a skill makes; a fully non-blocking variant is a later refinement
//! (the same trade-off the LLM client makes).

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use pan_core::pipeline::ExecError;
use pan_core::schema::{Capability, Value};
use pan_core::toolbox::CapabilityProvider;

use crate::snapshot::SnapshotStore;

/// Walk a directory tree recursively, yielding all files.
fn walk_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if root.is_dir() {
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            if let Ok(read) = std::fs::read_dir(&dir) {
                for entry in read.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        files.push(path);
                    }
                }
            }
        }
    } else if root.is_file() {
        files.push(root.to_path_buf());
    }
    Ok(files)
}

/// Simple glob matching: supports `*` (any chars except `/`) and `**` (any chars
/// including `/`). `?` matches a single non-`/` character. The pattern is
/// matched against the full path.
fn glob_match(pattern: &str, path: &str) -> bool {
    let pat_chars: Vec<char> = pattern.chars().collect();
    let path_chars: Vec<char> = path.chars().collect();
    glob_match_rec(&pat_chars, &path_chars, 0, 0)
}

fn glob_match_rec(pat: &[char], pth: &[char], pi: usize, si: usize) -> bool {
    if pi == pat.len() {
        return si == pth.len();
    }
    match pat[pi] {
        '*' => {
            // `**` matches everything including slashes
            if pi + 1 < pat.len() && pat[pi + 1] == '*' {
                // Skip `/` after `**` if present
                let next_pi = if pi + 2 < pat.len() && pat[pi + 2] == '/' {
                    pi + 3
                } else {
                    pi + 2
                };
                // `**` matches any remainder
                for s in si..=pth.len() {
                    if glob_match_rec(pat, pth, next_pi, s) {
                        return true;
                    }
                }
                false
            } else {
                // `*` matches any chars except `/`
                for s in si..=pth.len() {
                    if s < pth.len() && pth[s] == '/' {
                        continue;
                    }
                    if glob_match_rec(pat, pth, pi + 1, s) {
                        return true;
                    }
                    if s < pth.len() && pth[s] == '/' {
                        break;
                    }
                }
                false
            }
        }
        '?' => {
            if si < pth.len() && pth[si] != '/' {
                glob_match_rec(pat, pth, pi + 1, si + 1)
            } else {
                false
            }
        }
        c => {
            if si < pth.len() && pth[si] == c {
                glob_match_rec(pat, pth, pi + 1, si + 1)
            } else {
                false
            }
        }
    }
}

/// Filesystem capabilities confined to `root`.
pub struct FsCaps {
    root: PathBuf,
    /// Optional snapshot store for undo support. When set, `cap.fs.write`
    /// auto-snapshots the file before overwriting it.
    snapshot_store: Option<Arc<SnapshotStore>>,
}

impl FsCaps {
    /// Create an `cap.fs` component rooted at `root`. All paths are resolved
    /// relative to it and may not escape it.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            snapshot_store: None,
        }
    }

    /// Attach a snapshot store for automatic undo snapshots.
    pub fn with_snapshots(mut self, store: Arc<SnapshotStore>) -> Self {
        self.snapshot_store = Some(store);
        self
    }

    /// Resolve a caller-supplied relative path against the root, refusing
    /// anything that could escape it. This is the jail: absolute paths and any
    /// `..` component are rejected before touching the filesystem.
    fn jail(&self, rel: &str) -> Result<PathBuf, ExecError> {
        let p = Path::new(rel);
        if p.is_absolute() {
            return Err(ExecError(format!("absolute path `{rel}` is not allowed")));
        }
        for comp in p.components() {
            match comp {
                Component::ParentDir => {
                    return Err(ExecError(format!("`..` in `{rel}` is not allowed")))
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(ExecError(format!("`{rel}` must be relative")))
                }
                _ => {}
            }
        }
        Ok(self.root.join(p))
    }

    fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ExecError> {
        args.get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExecError(format!("`{key}` must be a string")))
    }
}

#[async_trait::async_trait]
impl CapabilityProvider for FsCaps {
    fn id(&self) -> &str {
        "cap.fs"
    }

    fn capabilities(&self) -> Vec<Capability> {
        let path_only = serde_json::json!({ "type": "object", "required": ["path"] });
        vec![
            Capability {
                id: "cap.fs.read".into(),
                summary: "read a UTF-8 file under the agent's root".into(),
                args_schema: path_only.clone(),
            },
            Capability {
                id: "cap.fs.write".into(),
                summary: "write a UTF-8 file under the agent's root".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "required": ["path", "content"]
                }),
            },
            Capability {
                id: "cap.fs.list".into(),
                summary: "list a directory under the agent's root".into(),
                args_schema: path_only,
            },
            Capability {
                id: "cap.fs.glob".into(),
                summary: "find files matching a glob pattern under the agent's root".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "required": ["pattern"]
                }),
            },
            Capability {
                id: "cap.fs.search".into(),
                summary: "search for text in files under the agent's root".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "required": ["query", "path"]
                }),
            },
            Capability {
                id: "cap.fs.undo".into(),
                summary: "restore a file from its most recent snapshot".into(),
                args_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "path to the file to restore" },
                        "snapshot_id": { "type": "string", "description": "optional specific snapshot id (default: latest)" }
                    },
                    "required": ["path"]
                }),
            },
        ]
    }

    async fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError> {
        let path = self.jail(Self::arg_str(args, "path")?)?;
        match capability {
            "cap.fs.read" => {
                let content =
                    std::fs::read_to_string(&path).map_err(|e| ExecError(format!("read: {e}")))?;
                Ok(serde_json::json!({ "content": content }))
            }
            "cap.fs.write" => {
                let content = Self::arg_str(args, "content")?;
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| ExecError(format!("mkdir: {e}")))?;
                }
                // Auto-snapshot before overwriting.
                if let Some(store) = &self.snapshot_store {
                    if path.exists() {
                        store.snapshot(&path).map_err(ExecError)?;
                    }
                }
                std::fs::write(&path, content).map_err(|e| ExecError(format!("write: {e}")))?;
                Ok(serde_json::json!({ "bytes": content.len() }))
            }
            "cap.fs.list" => {
                let mut entries = Vec::new();
                let dir = std::fs::read_dir(&path).map_err(|e| ExecError(format!("list: {e}")))?;
                for entry in dir {
                    let entry = entry.map_err(|e| ExecError(format!("list: {e}")))?;
                    entries.push(entry.file_name().to_string_lossy().into_owned());
                }
                entries.sort();
                Ok(serde_json::json!({ "entries": entries }))
            }
            "cap.fs.glob" => {
                let pattern = Self::arg_str(args, "pattern")?;
                let jailed = self.jail(pattern)?;
                let pattern_str = jailed.to_string_lossy().into_owned();
                let mut matches = Vec::new();
                // Walk the root directory and match paths against the glob pattern.
                if let Ok(walk) = walk_files(&self.root) {
                    for entry in walk {
                        let entry_str = entry.to_string_lossy().into_owned();
                        if glob_match(&pattern_str, &entry_str) {
                            if let Ok(rel) = entry.strip_prefix(&self.root) {
                                matches.push(rel.to_string_lossy().into_owned());
                            }
                        }
                    }
                }
                matches.sort();
                Ok(serde_json::json!({ "matches": matches }))
            }
            "cap.fs.search" => {
                let query = Self::arg_str(args, "query")?;
                let search_path = self.jail(Self::arg_str(args, "path")?)?;
                let mut results = Vec::new();
                if search_path.is_dir() {
                    if let Ok(walk) = walk_files(&search_path) {
                        for entry in walk {
                            if entry.is_file() {
                                if let Ok(content) = std::fs::read_to_string(&entry) {
                                    for (i, line) in content.lines().enumerate() {
                                        if line.contains(query) {
                                            if let Ok(rel) = entry.strip_prefix(&self.root) {
                                                results.push(serde_json::json!({
                                                    "file": rel.to_string_lossy(),
                                                    "line": i + 1,
                                                    "text": line,
                                                }));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else if search_path.is_file() {
                    if let Ok(content) = std::fs::read_to_string(&search_path) {
                        for (i, line) in content.lines().enumerate() {
                            if line.contains(query) {
                                if let Ok(rel) = search_path.strip_prefix(&self.root) {
                                    results.push(serde_json::json!({
                                        "file": rel.to_string_lossy(),
                                        "line": i + 1,
                                        "text": line,
                                    }));
                                }
                            }
                        }
                    }
                }
                Ok(serde_json::json!({ "matches": results }))
            }
            "cap.fs.undo" => {
                let store = self.snapshot_store.as_ref().ok_or_else(|| {
                    ExecError("snapshot store not configured (no `snapshot_root` setting)".into())
                })?;
                if args.get("_list").and_then(|v| v.as_bool()).unwrap_or(false) {
                    let metas = store.list(&path).map_err(ExecError)?;
                    let snapshots: Vec<Value> = metas
                        .into_iter()
                        .map(|m| serde_json::json!({ "id": m.id, "timestamp": m.timestamp, "path": m.path }))
                        .collect();
                    return Ok(serde_json::json!({ "snapshots": snapshots }));
                }
                let snapshot_id = args.get("snapshot_id").and_then(|v| v.as_str());
                let result = match snapshot_id {
                    Some(id) => {
                        store.restore(&path, id).map_err(ExecError)?;
                        serde_json::json!({ "restored": path.to_string_lossy(), "snapshot_id": id })
                    }
                    None => {
                        let id = store.restore_latest(&path).map_err(ExecError)?;
                        serde_json::json!({ "restored": path.to_string_lossy(), "snapshot_id": id })
                    }
                };
                Ok(result)
            }
            other => Err(ExecError(format!("cap.fs has no `{other}`"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pan_cap_fs_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let fs = FsCaps::new(temp_root());
        fs.execute(
            "cap.fs.write",
            &serde_json::json!({ "path": "a/b.txt", "content": "hi" }),
        )
        .await
        .unwrap();
        let got = fs
            .execute("cap.fs.read", &serde_json::json!({ "path": "a/b.txt" }))
            .await
            .unwrap();
        assert_eq!(got["content"], "hi");
    }

    #[tokio::test]
    async fn path_traversal_is_refused_even_within_cap_fs() {
        let fs = FsCaps::new(temp_root());
        let err = fs
            .execute("cap.fs.read", &serde_json::json!({ "path": "../secrets" }))
            .await
            .unwrap_err();
        assert!(err.0.contains(".."), "traversal must be refused: {}", err.0);
    }

    #[tokio::test]
    async fn absolute_paths_are_refused() {
        let fs = FsCaps::new(temp_root());
        let err = fs
            .execute("cap.fs.read", &serde_json::json!({ "path": "/etc/passwd" }))
            .await
            .unwrap_err();
        assert!(err.0.contains("absolute"), "got: {}", err.0);
    }
}
