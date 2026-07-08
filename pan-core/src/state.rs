//! # StateSlot trait — the storage abstraction shared by `state.memory` and `state.file`.
//!
//! Both Wave 1's in-memory store and Wave 2's file-backed store implement this
//! trait, so the rest of Pan writes state through a single interface. A client
//! (e.g. `cap.state_write`) receives a `&dyn StateSlot` and never knows whether
//! the backing is memory or disk.
//!
//! ## Wave 1 stance
//!
//! The trait includes `load` and `flush` so callers don't need to know which
//! variant they hold. The in-memory implementation keeps both as no-ops; the
//! file implementation gives them real behaviour. This follows the principle
//! (manifest §13.2) that the interface is the same — only durability changes.

use crate::schema::Value;

/// A key/value state slot. Thread-safe (all methods take `&self`).
///
/// Keys are flat dotted strings (e.g. `"soul.last_conversation"`). Both memory
/// and file backends guarantee that a `write` is immediately visible to a
/// subsequent `read` on the same instance — the difference is whether the
/// value survives a process restart.
pub trait StateSlot: Send + Sync {
    /// Load state from the durable backing store. For MemoryState this is a
    /// no-op; for StateFile it reads the JSON file. Idempotent — safe to call
    /// multiple times.
    fn load(&self) -> Result<(), String> {
        Ok(())
    }

    /// Flush in-memory state to the durable backing store. For MemoryState
    /// this is a no-op; for StateFile it atomically rewrites the JSON file.
    /// Idempotent — safe to call when nothing has changed.
    fn flush(&self) -> Result<(), String> {
        Ok(())
    }

    /// Write a value at `path`. Overwrites any existing value at the same key.
    fn write(&self, path: &str, value: Value);

    /// Read the value at `path`, or `None` if absent.
    fn read(&self, path: &str) -> Option<Value>;

    /// List all stored keys.
    fn keys(&self) -> Vec<String>;
}
