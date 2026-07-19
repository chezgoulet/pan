//! # The skill wire protocol — newline-delimited JSON over stdin/stdout.
//!
//! One JSON object per line. The skill's **stdout** carries [`FromSkill`]
//! messages to the host; the host writes result messages back to the skill's
//! **stdin**. The skill's **stderr** is out-of-band diagnostics (captured by the
//! runner, never parsed). Input to the skill is passed once, via the
//! `PAN_SKILL_INPUT` environment variable, so the stdin/stdout channel is purely
//! the invoke ↔ result conversation.
//!
//! The conversation is single-flight: a skill emits one `invoke` and blocks for
//! its `result` before doing anything else, so the host never has more than one
//! outstanding request per skill. A `return` is terminal.

use pan_core::schema::Value;
use serde::Deserialize;

/// A line the skill wrote to its stdout.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromSkill {
    /// "Run this capability with these args and send me the result." The host
    /// routes it through the governed pipeline under the skill's bound scope.
    Invoke {
        /// Correlates the `result` back to this call. The host echoes it.
        id: u64,
        capability: String,
        #[serde(default)]
        args: Value,
    },
    /// "I'm done; here is my return value." Terminal — the skill then exits.
    Return {
        #[serde(default)]
        value: Value,
    },
}

/// The host's reply to an [`FromSkill::Invoke`], serialized to the skill's stdin
/// as `{"type":"result","id":N,"ok":true,"value":…}` or
/// `{"type":"result","id":N,"ok":false,"error":{"kind":…,"message":…}}`.
///
/// Built directly with `serde_json` in the runner (see `result_line`); this
/// struct documents the shape the Python `pan.invoke` expects.
pub const RESULT_TYPE: &str = "result";
