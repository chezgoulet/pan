//! # pan-skill — the Python skill runtime.
//!
//! A **skill** is a plain Python program. It reaches the world only by asking the
//! host to invoke capabilities on its behalf, and every such request is run
//! through pan-core's governed pipeline under the scope the skill was granted.
//! This crate is the bridge: it spawns the subprocess, hands it the tiny `pan`
//! client library ([`runner::PAN_PY`]), and services its invokes against a
//! [`ScopedInvoker`](pan_core::invoker::ScopedInvoker).
//!
//! This is the full resolution of the "a skill is not an Executor" point in ADR
//! 0001 (D2): a skill emitting `cap.invoke(...)` is a *governed invoker* driven
//! across a process boundary, not a leaf effector. The transport is thin; the
//! governance guarantee lives in Rust, in the pipeline the invoker routes
//! through — the subprocess holds no capability object of its own.
//!
//! ```no_run
//! # async fn demo(invoker: &dyn pan_core::invoker::ScopedInvoker) -> Result<(), Box<dyn std::error::Error>> {
//! use pan_skill::SkillRunner;
//! use std::path::Path;
//!
//! let runner = SkillRunner::new("/var/lib/pan/skill-lib")?;
//! let out = runner
//!     .run(Path::new("summarize.py"), &serde_json::json!({ "path": "notes.md" }), invoker)
//!     .await?;
//! println!("skill returned: {out}");
//! # Ok(()) }
//! ```

pub mod protocol;
pub mod runner;

pub use protocol::FromSkill;
pub use runner::{SkillError, SkillRunner, PAN_PY};
