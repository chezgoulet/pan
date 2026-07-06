//! # `gov.allow` — trivial always-allow governor (Wave 1).
//!
//! The govern stage that lets the pipeline run end to end during the walking
//! skeleton. It is **intentionally unsafe**: it permits every effect. Wave 4
//! replaces it with `gov.policy` (allow/deny/approval). Kept as a separate,
//! named plugin — not an inline closure — so the lifecycle can report exactly
//! which governor is active (and so a deployment fails loud if it accidentally
//! ships to chat with `gov.allow` still registered).

use crate::pipeline::{Governor, Verdict};
use crate::registry::Plugin;
use crate::schema::Value;

pub struct Allow {
    /// When false, refuses everything (a kill-switch for safety during dev).
    pub enabled: bool,
}

impl Default for Allow {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Allow {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Governor for Allow {
    fn id(&self) -> &str {
        "gov.allow"
    }

    fn govern(&self, capability: &str, _args: &Value) -> Verdict {
        if self.enabled {
            Verdict::Allow
        } else {
            Verdict::Deny {
                reason: format!("gov.allow disabled; `{capability}` refused (kill-switch)"),
            }
        }
    }
}

impl Plugin for Allow {
    fn id(&self) -> &str {
        "gov.allow"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_when_enabled() {
        let g = Allow::new();
        assert_eq!(g.govern("cap.shell", &Value::Null), Verdict::Allow);
    }

    #[test]
    fn denies_when_kill_switch_on() {
        let g = Allow { enabled: false };
        assert!(matches!(g.govern("cap.shell", &Value::Null), Verdict::Deny { .. }));
    }
}
