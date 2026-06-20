//! Compile-fail guards for the core's structural invariants.
//!
//! These tests assert that certain *bypasses do not compile*. Because a normal
//! `#[test]` cannot observe a compile error, each guard is recorded here as a
//! standalone source file under `tests/compile-fail/`, together with the exact
//! rustc error it must produce. A `trybuild` harness can run them automatically;
//! until that dev-dependency is wired, `verify.sh` at the repo root compiles each
//! and asserts failure. The guarded invariants:
//!
//! 1. `governed_bypass.rs`  — cannot construct `Governed` (skip the govern stage).
//!    Expected: E0451 (private field `request`).
//! 2. `handle_write.rs`     — cannot write through a read-only `MemoryQuery`.
//!    Expected: E0599 (no method `remember`).
//! 3. `handle_downcast.rs`  — cannot name the private `QueryHandle` to recover the
//!    writer. Expected: E0412 (cannot find type `QueryHandle`).
//!
//! If any of these ever COMPILES, a core boundary has regressed.

// This file intentionally contains no runnable tests; it documents the guard set
// and exists so the invariant is discoverable from the test tree.
#[test]
fn compile_fail_guards_are_documented() {
    // The guard sources live in tests/compile-fail/ and are checked by verify.sh.
    // See module docs above.
}
