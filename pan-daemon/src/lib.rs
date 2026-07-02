//! # Pan daemon — the Soul Protocol server.
//!
//! `pan serve` speaks the Soul Protocol over a single TCP loopback connection,
//! NDJSON framed. It hosts one or more *souls* (the daemon's runtime mind-state
//! for each NPC the host manages), receives `perceive` events for each, decides
//! what to do (via the rules provider in M1), and ships the `decision` back.
//!
//! Architectural boundary: this crate is the wire-only layer. It knows about
//! Pan's vocabulary types (re-exported from [`pan_core::schema`]) and about the
//! Soul Protocol envelope ([`wire`]). It does NOT know which host is talking to
//! it, what content ids the host uses, or anything about the host's domain —
//! the wire IS the contract.
//!
//! ## Module map
//!
//! - [`wire`] — envelope + body serde types, the JSON shape of every line.
//! - [`governor`] — `ResolveGovernor`: rejects `Invoke` of a capability that
//!   isn't on the host's registered list, with `error code: "unknown_capability"`.
//! - [`session`] — per-connection session: hello/welcome, registered
//!   capabilities, instantiated souls, the perceive→decision loop.
//! - [`server`] — TCP loopback listener, NDJSON framing, single-connection
//!   lifecycle (a new connect drops the old one cleanly).
//! - [`conformance`] — fixture loader + validator; used by the conformance test
//!   in `tests/conformance.rs` to assert the wire schema and Pan's serde types
//!   agree on every message in the shared Godot fixture set.

pub mod conformance;
pub mod governor;
pub mod llm;
pub mod server;
pub mod session;
pub mod wire;

// Re-export the Pan core vocabulary at the daemon root, so callers building on
// pan-daemon have one import: `use pan_daemon::{Goal, Capability, ...}`.
pub use pan_core::schema;
