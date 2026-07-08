//! # `gov.policy` — governance policy with pairing/allowlist for inbound channels.
//!
//! Replaces `gov.allow` in Wave 4/5. Has two roles:
//!
//! 1. **Capability governance** (as a [`Governor`]): allow/deny rules for
//!    capabilities. The admission layer checks pairing before the LLM is called,
//!    so unpaired users never incur LLM cost. At the `govern` stage, this plugin
//!    applies capability-level rules.
//!
//! 2. **Channel allowlist** (admission): which senders are paired for inbound
//!    channels. By default inbound channels are untrusted. Only paired senders
//!    reach the agent's LLM. Unpaired senders get "not authorized" without LLM
//!    cost — the admission layer (CLI/gateway) checks [`Policy::is_paired`]
//!    before entering the loop.
//!
//! ## Pairing flow
//!
//! 1. An authorized user generates a pairing code via `cap.pair.invite`
//! 2. The new user sends the code as their first message
//! 3. The admission layer detects the code, validates it, and adds the user
//!    to the allowlist
//! 4. On subsequent messages the user is recognized as paired
//!
//! ## Design notes
//!
//! - The allowlist is in-process memory. A durable production build would
//!   persist it through [`crate::handles::MemoryStore`] or a file-backed store,
//!   but for Wave 5 the walking skeleton's in-memory state is sufficient.
//! - Pairing codes are single-use: once a user pairs, the code is consumed.
//! - The kill-switch (`enabled`) mirrors `gov.allow`'s safety refuser for
//!   development convenience.

use crate::pipeline::{Governor, Verdict};
use crate::registry::Plugin;
use crate::schema::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Governance policy with a channel allowlist and pairing mechanism.
///
/// ## Thread safety
///
/// `Policy` is `Send + Sync` (all interior state is `Mutex`-protected),
/// so it satisfies the `Governor` and `Plugin` trait bounds.
pub struct Policy {
    /// Paired (allowlisted) users who may reach the agent's LLM.
    paired_users: Mutex<HashSet<String>>,

    /// Active pairing codes: code -> admin user id who generated it.
    /// A code is consumed on first successful use.
    pairing_codes: Mutex<HashMap<String, String>>,

    /// When false, refuses every effect (kill-switch for safety during dev).
    pub enabled: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            paired_users: Mutex::new(HashSet::new()),
            pairing_codes: Mutex::new(HashMap::new()),
            enabled: true,
        }
    }
}

impl Policy {
    pub fn new() -> Self {
        Self::default()
    }

    // ------------------------------------------------------------------
    // Allowlist (admission-layer interface)
    // ------------------------------------------------------------------

    /// Is this user paired? Called by the admission layer before the LLM.
    pub fn is_paired(&self, user_id: &str) -> bool {
        self.paired_users.lock().unwrap().contains(user_id)
    }

    /// Authorize a user: add to the allowlist.
    pub fn authorize(&self, user_id: &str) {
        self.paired_users.lock().unwrap().insert(user_id.to_string());
    }

    /// Revoke a user's access.
    pub fn revoke(&self, user_id: &str) {
        self.paired_users.lock().unwrap().remove(user_id);
    }

    /// Number of paired users (for diagnostics).
    pub fn paired_count(&self) -> usize {
        self.paired_users.lock().unwrap().len()
    }

    /// List all paired user ids (for diagnostics).
    pub fn paired_users(&self) -> Vec<String> {
        self.paired_users.lock().unwrap().iter().cloned().collect()
    }

    /// Inject a pre-shared pairing code (for env-var / config bootstrap).
    /// `admin_id` is the user who "generated" the code.
    pub fn inject_code(&self, code: &str, admin_id: &str) {
        self.pairing_codes
            .lock()
            .unwrap()
            .insert(code.to_string(), admin_id.to_string());
    }

    // ------------------------------------------------------------------
    // Pairing codes
    // ------------------------------------------------------------------

    /// Generate a new single-use pairing code on behalf of `admin_user`.
    /// Returns the code as a string the new user can send to pair.
    pub fn generate_code(&self, admin_user: &str) -> String {
        let code = self.make_code(admin_user);
        self.pairing_codes
            .lock()
            .unwrap()
            .insert(code.clone(), admin_user.to_string());
        code
    }

    /// Attempt to pair a user with a code. Returns `true` on success.
    /// The code is consumed (single-use) regardless of success, to prevent
    /// replay attempts.
    pub fn pair_with_code(&self, user_id: &str, code: &str) -> bool {
        let mut codes = self.pairing_codes.lock().unwrap();
        if codes.remove(code).is_some() {
            self.paired_users.lock().unwrap().insert(user_id.to_string());
            true
        } else {
            false
        }
    }

    /// Number of outstanding pairing codes (for diagnostics).
    pub fn pending_codes(&self) -> usize {
        self.pairing_codes.lock().unwrap().len()
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    fn make_code(&self, admin_user: &str) -> String {
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        // Hex fragment: first 8 hex characters of the nanosecond timestamp.
        let hex = format!("{:016x}", epoch);
        // Trim admin name to first 4 chars for readability.
        let prefix: String = admin_user.chars().take(4).collect();
        // Use the lower 8 hex chars (least significant bits) which change with
        // each nanosecond tick, unlike the upper 8 which are stable across
        // consecutive calls.
        format!("pair-{}-{}", prefix, &hex[(hex.len() - 8).max(0)..])
    }
}

// -----------------------------------------------------------------------
// Governor implementation
// -----------------------------------------------------------------------

impl Governor for Policy {
    fn id(&self) -> &str {
        "gov.policy"
    }

    fn govern(&self, capability: &str, _args: &Value) -> Verdict {
        if !self.enabled {
            return Verdict::Deny {
                reason: format!(
                    "gov.policy disabled; `{capability}` refused (kill-switch)"
                ),
            };
        }

        // Capability-level governance rules.
        //
        // The admission layer already handles channel allowlisting (unpaired
        // senders never reach the LLM). At the govern stage we apply further
        // capability restrictions:
        //
        // - `cap.pair.invite` is always allowed (admin pairing)
        // - All other capabilities are allowed when the agent is enabled
        //
        // Wave 4+ will extend this with a configurable rules table
        // (allow/deny/require-approval) and integration with `gov.approval`
        // for dangerous invokes.

        // No restrictions yet beyond the kill-switch.
        Verdict::Allow
    }
}

// -----------------------------------------------------------------------
// Plugin implementation
// -----------------------------------------------------------------------

impl Plugin for Policy {
    fn id(&self) -> &str {
        "gov.policy"
    }
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Allowlist tests -------------------------------------------------

    #[test]
    fn new_policy_has_no_paired_users() {
        let p = Policy::new();
        assert!(!p.is_paired("alice"));
        assert_eq!(p.paired_count(), 0);
    }

    #[test]
    fn authorized_user_is_paired() {
        let p = Policy::new();
        p.authorize("alice");
        assert!(p.is_paired("alice"));
        assert!(!p.is_paired("bob"));
        assert_eq!(p.paired_count(), 1);
    }

    #[test]
    fn revoked_user_is_no_longer_paired() {
        let p = Policy::new();
        p.authorize("alice");
        assert!(p.is_paired("alice"));
        p.revoke("alice");
        assert!(!p.is_paired("alice"));
    }

    #[test]
    fn paired_users_lists_all() {
        let p = Policy::new();
        p.authorize("alice");
        p.authorize("bob");
        let mut users = p.paired_users();
        users.sort();
        assert_eq!(users, vec!["alice", "bob"]);
    }

    // -- Pairing code tests ----------------------------------------------

    #[test]
    fn generate_code_creates_valid_code() {
        let p = Policy::new();
        let code = p.generate_code("admin");
        assert!(code.starts_with("pair-"));
        assert!(code.len() > 10); // "pair-" + prefix + "-" + 8 hex = at least 15
        assert_eq!(p.pending_codes(), 1);
    }

    #[test]
    fn pair_with_code_succeeds_and_consumes_code() {
        let p = Policy::new();
        let code = p.generate_code("admin");
        assert!(p.pair_with_code("alice", &code));
        assert!(p.is_paired("alice"));
        // Code consumed: trying again fails.
        assert!(!p.pair_with_code("bob", &code));
        assert!(!p.is_paired("bob"));
    }

    #[test]
    fn pair_with_invalid_code_fails() {
        let p = Policy::new();
        assert!(!p.pair_with_code("alice", "pair-admin-00000000"));
        assert!(!p.is_paired("alice"));
    }

    #[test]
    fn code_is_single_use() {
        let p = Policy::new();
        let code = p.generate_code("admin");
        assert!(p.pair_with_code("alice", &code));
        // Second use with different user fails (code already consumed).
        assert!(!p.pair_with_code("bob", &code));
        assert!(p.is_paired("alice"));
        assert!(!p.is_paired("bob"));
    }

    #[test]
    fn multiple_codes_all_valid() {
        let p = Policy::new();
        let c1 = p.generate_code("admin");
        let c2 = p.generate_code("admin");
        assert_eq!(p.pending_codes(), 2);
        assert!(p.pair_with_code("alice", &c1));
        assert!(p.pair_with_code("bob", &c2));
        assert_eq!(p.paired_count(), 2);
        assert_eq!(p.pending_codes(), 0);
    }

    #[test]
    fn inject_code_adds_to_pending() {
        let p = Policy::new();
        p.inject_code("hello-world", "bootstrap");
        assert_eq!(p.pending_codes(), 1);
        assert!(p.pair_with_code("new_user", "hello-world"));
        assert!(p.is_paired("new_user"));
        assert_eq!(p.pending_codes(), 0);
    }

    // -- Governor tests --------------------------------------------------

    #[test]
    fn governor_allows_when_enabled() {
        let p = Policy::new();
        assert_eq!(p.govern("cap.shell", &Value::Null), Verdict::Allow);
        assert_eq!(p.govern("cap.pair.invite", &Value::Null), Verdict::Allow);
    }

    #[test]
    fn governor_denies_when_kill_switch_on() {
        let p = Policy { enabled: false, ..Default::default() };
        let result = p.govern("cap.shell", &Value::Null);
        assert!(matches!(result, Verdict::Deny { .. }));
        if let Verdict::Deny { reason } = result {
            assert!(reason.contains("kill-switch"), "reason: {reason}");
        }
    }

    #[test]
    fn governor_denies_all_when_kill_switch_on() {
        let p = Policy { enabled: false, ..Default::default() };
        let result = p.govern("anything", &Value::Null);
        assert!(matches!(result, Verdict::Deny { .. }));
    }

    #[test]
    fn plugin_id_is_gov_policy() {
        let p = Policy::new();
        assert_eq!(Plugin::id(&p), "gov.policy");
    }

    // -- Admission flow integration test ---------------------------------

    #[test]
    fn full_pairing_flow() {
        let p = Policy::new();

        // Step 1: Admin generates a code.
        let code = p.generate_code("admin");
        assert_eq!(p.pending_codes(), 1);

        // Step 2: Before pairing, alice is not paired.
        assert!(!p.is_paired("alice"));

        // Step 3: Alice sends the code → paired.
        assert!(p.pair_with_code("alice", &code));
        assert!(p.is_paired("alice"));
        assert_eq!(p.pending_codes(), 0);

        // Step 4: Bob arrives with no code → stays unpaired.
        assert!(!p.is_paired("bob"));

        // Step 5: Bob gets a code from admin.
        let code2 = p.generate_code("admin");
        assert!(p.pair_with_code("bob", &code2));
        assert!(p.is_paired("bob"));

        // Step 6: Both paired, only two users.
        assert_eq!(p.paired_count(), 2);
    }
}
