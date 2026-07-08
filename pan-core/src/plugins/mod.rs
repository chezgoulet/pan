//! # Wave 1 plugins — the walking skeleton.
//!
//! These are the smallest plugins that make Pan *do something real* end to end
//! (manifest Wave 1, exit test: type a command → model emits `Invoke(cap.shell)`
//! → runs → reply printed → visible in logs). None require an external API key;
//! `cap.shell` + `exec.local` + `channel.cli` + `state.memory` + `obs.logging` +
//! `gov.allow` reach a runnable agent with the stub provider, and a real model
//! once `provider.llm` is wired in.
//!
//! Each plugin implements one of the core slots:
//! - `Governor` (`gov.allow`) — the trivial always-allow govern stage.
//! - `Executor` (`exec.local`) — performs `cap.shell` (and any other) in-process.
//! - `Plugin` (`state.memory`, `obs.logging`) — lifecycle + observability.

pub mod exec_local;
pub mod gov_allow;
pub mod obs_admission;
pub mod obs_logging;
pub mod sched_cron;
pub mod sched_eventbus;
pub mod state_memory;
