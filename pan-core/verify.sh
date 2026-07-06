#!/usr/bin/env bash
# Full verification for Pan (Wave 0 core + Wave 1 walking skeleton):
#   1. build + unit/integration tests across the workspace (positive guarantees)
#   2. the compile-fail guards (negative guarantees: pipeline bypasses must NOT
#      compile) — exercised by pan-core's `tests/compile_fail.rs` integration
#      test, which shells out to rustc on tests/compile-fail/*.rs and asserts
#      each is rejected. Run via cargo test so dependency resolution matches the
#      workspace exactly.
#
# A core boundary regresses the moment a compile-fail guard starts compiling.
set -uo pipefail
cd "$(dirname "$0")/.."   # workspace root (this script lives in pan-core/)

echo "==> cargo test (workspace, incl. compile-fail guards)"
cargo test --quiet || { echo "FAIL: tests"; exit 1; }

echo "==> cargo build (all targets, incl. pan-cli)"
cargo build --quiet || { echo "FAIL: build"; exit 1; }

echo "==> ALL GUARANTEES HOLD"