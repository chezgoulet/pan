use crate::registry::Plugin;
use crate::schema::Fragment;
use std::sync::Mutex;

type Turn = (String, String);

#[derive(Default)]
pub struct ContextHistory {
    max_turns: usize,
    history: Mutex<Vec<Turn>>,
}

impl ContextHistory {
    pub fn new(max_turns: usize) -> Self {
        Self {
            max_turns,
            history: Mutex::new(Vec::new()),
        }
    }

    pub fn record(&self, role: &str, content: String) {
        let mut h = self.history.lock().unwrap();
        h.push((role.to_string(), content));
        if h.len() > self.max_turns {
            h.remove(0);
        }
    }

    pub fn render(&self) -> Fragment {
        let h = self.history.lock().unwrap();
        let body = h
            .iter()
            .map(|(role, content)| format!("{role}: {}", content))
            .collect::<Vec<_>>()
            .join("\n");
        
        Fragment {
            channel: "history".into(),
            body,
        }
    }
}

impl Plugin for ContextHistory {
    fn id(&self) -> &str {
        "context.history"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_prunes() {
        let ch = ContextHistory::new(2);
        ch.record("user", "hi".into());
        ch.record("assistant", "hello".into());
        ch.record("user", "how are you?".into());
        
        let frag = ch.render();
        // Should only contain the last 2 turns
        assert!(!frag.body.contains("hi"));
        assert!(frag.body.contains("hello"));
        assert!(frag.body.contains("how are you?"));
    }
}