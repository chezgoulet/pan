use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Metadata for a single file snapshot.
#[derive(Debug, Clone)]
pub struct SnapshotMeta {
    pub id: String,
    pub timestamp: u64,
    pub path: String,
}

/// A store of file snapshots for undo functionality.
///
/// Each snapshot lives at `{root}/{escaped_path}/{unix_nanos}.snap`.
/// `escaped_path` replaces `/` with `_` (safe on all platforms).
pub struct SnapshotStore {
    root: PathBuf,
}

impl SnapshotStore {
    /// Create a store rooted at `root`. All snapshots live under this directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Snapshot `path` and return a snapshot id (unix nanos).
    pub fn snapshot(&self, path: &Path) -> Result<String, String> {
        let content = std::fs::read(path).map_err(|e| format!("snapshot read: {e}"))?;
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let id = format!("{ts}");
        let snap_dir = self.snap_dir_for(path);
        std::fs::create_dir_all(&snap_dir).map_err(|e| format!("snapshot mkdir: {e}"))?;
        std::fs::write(snap_dir.join(format!("{id}.snap")), &content)
            .map_err(|e| format!("snapshot write: {e}"))?;
        Ok(id)
    }

    /// Restore the latest snapshot for `path`. Returns the restored snapshot id.
    pub fn restore_latest(&self, path: &Path) -> Result<String, String> {
        let meta = self
            .list(path)?
            .into_iter()
            .max_by_key(|m| m.timestamp)
            .ok_or_else(|| format!("no snapshots for `{}`", path.display()))?;
        self.restore(path, &meta.id)?;
        Ok(meta.id)
    }

    /// Restore a specific snapshot for `path` by id.
    pub fn restore(&self, path: &Path, id: &str) -> Result<(), String> {
        let snap_path = self.snap_dir_for(path).join(format!("{id}.snap"));
        let content = std::fs::read(&snap_path).map_err(|e| format!("restore read: {e}"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("restore mkdir: {e}"))?;
        }
        std::fs::write(path, &content).map_err(|e| format!("restore write: {e}"))?;
        Ok(())
    }

    /// List all snapshots for `path`, newest first.
    pub fn list(&self, path: &Path) -> Result<Vec<SnapshotMeta>, String> {
        let dir = self.snap_dir_for(path);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut metas = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("snapshot list: {e}"))? {
            let entry = entry.map_err(|e| format!("snapshot entry: {e}"))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(nanos_str) = name.strip_suffix(".snap") {
                if let Ok(nanos) = nanos_str.parse::<u64>() {
                    metas.push(SnapshotMeta {
                        id: nanos_str.to_string(),
                        timestamp: nanos / 1_000_000_000, // nanos → secs
                        path: path.to_string_lossy().into_owned(),
                    });
                }
            }
        }
        // Sort newest first
        metas.sort_by_key(|b| std::cmp::Reverse(b.timestamp));
        Ok(metas)
    }

    /// The directory holding snapshots for a given file path.
    fn snap_dir_for(&self, path: &Path) -> PathBuf {
        let escaped = path.to_string_lossy().replace(['/', '\\'], "_");
        self.root.join(escaped)
    }
}
