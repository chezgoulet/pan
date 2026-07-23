use std::path::PathBuf;

use pan_core::schema::Value;

/// One stored turn in the session file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionTurn {
    pub ts: u64,
    pub goal_id: String,
    pub objective: String,
    pub expressed: Vec<String>,
    pub results: Vec<(String, Value)>,
}

/// Persistent session store. Writes turns as JSONL to a file so conversations
/// survive restarts. The associated [`SessionContextAssembler`] reads the file
/// back to reconstruct history on the next run.
pub struct SessionStore {
    path: PathBuf,
    /// Cache of loaded turns to avoid re-reading the file on every
    /// assemble() while the session is active.
    cached: std::sync::Mutex<Vec<SessionTurn>>,
    max_turns: usize,
}

impl SessionStore {
    /// Open or create a session file at `path`. Loads existing turns into
    /// an in-memory cache.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let cached = std::sync::Mutex::new(Self::read_file(&path));
        Self {
            path,
            cached,
            max_turns: 100,
        }
    }

    /// Set the maximum number of turns to keep in memory for reload.
    pub fn with_max_turns(mut self, n: usize) -> Self {
        self.max_turns = n;
        self
    }

    /// Append a turn to the session file and cache.
    pub fn append(
        &self,
        goal: &pan_core::schema::Goal,
        expressed: &[String],
        results: &[(String, Value)],
    ) {
        let turn = SessionTurn {
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            goal_id: goal.id.clone(),
            objective: goal.objective.clone(),
            expressed: expressed.to_vec(),
            results: results.to_vec(),
        };
        // Append to file.
        if let Ok(line) = serde_json::to_string(&turn) {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                use std::io::Write;
                let _ = writeln!(file, "{line}");
            }
        }
        // Update cache.
        if let Ok(mut cache) = self.cached.lock() {
            cache.push(turn);
            while cache.len() > self.max_turns {
                cache.remove(0);
            }
        }
    }

    /// Load the cached turns, oldest first, up to `max_turns`.
    pub fn turns(&self) -> Vec<SessionTurn> {
        if let Ok(cache) = self.cached.lock() {
            let len = cache.len();
            let start = len.saturating_sub(self.max_turns);
            cache[start..].to_vec()
        } else {
            Vec::new()
        }
    }

    fn read_file(path: &PathBuf) -> Vec<SessionTurn> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        text.lines()
            .filter_map(|line| serde_json::from_str::<SessionTurn>(line).ok())
            .collect()
    }
}
