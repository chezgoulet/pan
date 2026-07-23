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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trip() {
        let dir = std::env::temp_dir().join(format!("pan_session_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("session.jsonl");
        let store = SessionStore::new(&path);

        let goal = pan_core::schema::Goal {
            id: "t1".into(),
            revision: 0,
            objective: "write a file".into(),
            trigger: pan_core::schema::Trigger::Utterance {
                from: "user".into(),
                content: "write a file".into(),
            },
        };
        store.append(
            &goal,
            &["done".into()],
            &[("cap.fs.write".into(), serde_json::json!({"bytes": 42}))],
        );

        let turns = store.turns();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].objective, "write a file");
        assert_eq!(turns[0].expressed, vec!["done"]);
        assert_eq!(turns[0].results.len(), 1);

        // Reload from file.
        let store2 = SessionStore::new(&path);
        let turns2 = store2.turns();
        assert_eq!(turns2.len(), 1);
        assert_eq!(turns2[0].objective, "write a file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_max_turns_trim() {
        let dir = std::env::temp_dir().join(format!("pan_session_trim_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("session.jsonl");
        let store = SessionStore::new(&path).with_max_turns(3);

        let goal = pan_core::schema::Goal {
            id: "t".into(),
            revision: 0,
            objective: "msg".into(),
            trigger: pan_core::schema::Trigger::Tick { sequence: 0 },
        };
        for i in 0..5 {
            store.append(&goal, &[format!("reply {i}")], &[]);
        }

        let turns = store.turns();
        assert_eq!(turns.len(), 3, "should keep only max_turns");
        assert_eq!(turns[0].expressed[0], "reply 2");
        assert_eq!(turns[2].expressed[0], "reply 4");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
