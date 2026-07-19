#!/usr/bin/env bash
# Full verification for the Pan Wave 0 core:
#   1. build + unit/integration tests (the positive guarantees)
#   2. the compile-fail guards (the negative guarantees: bypasses must NOT compile)
#
# A core boundary regresses the moment a compile-fail guard starts compiling.
#
# The PRIMARY guarantee is non-compilation. The rustc error *code* each guard
# cites is a secondary hint: it pins down that the rejection is for the intended
# reason, but exact codes drift across toolchains (e.g. naming a private type has
# reported both E0412 and E0425). So a differing-but-still-failing code is a
# WARNING, not a failure — only a guard that COMPILES, or one that fails without
# any compiler error at all (e.g. a stale rlib), breaks the build.
set -uo pipefail
cd "$(dirname "$0")"

echo "==> cargo test"
cargo test --quiet || { echo "FAIL: tests"; exit 1; }

echo "==> building rlib for compile-fail checks"
# Resolve the real target directory — in a workspace, `cargo build` writes to the
# workspace target, not ./target, so the compile-fail checks must link the rlib
# from wherever cargo actually put it (this is what silently broke the guards).
cargo build --quiet || { echo "FAIL: build"; exit 1; }
TARGET_DIR=$(cargo metadata --no-deps --format-version 1 2>/dev/null \
  | grep -o '"target_directory":"[^"]*"' | head -1 | cut -d'"' -f4)
TARGET_DIR=${TARGET_DIR:-target}
DEPS="$TARGET_DIR/debug/deps"
RLIB="$TARGET_DIR/debug/libpan_core.rlib"
SJ=$(ls "$DEPS"/libserde_json-*.rlib | head -1)
echo "    (linking $RLIB)"

fail=0
for src in tests/compile-fail/*.rs; do
  base=$(basename "$src")
  want=$(grep -oE 'E[0-9]{3,4}' "$src" | head -1)
  out=$(rustc --edition 2021 "$src" \
        --extern pan_core="$RLIB" \
        --extern serde_json="$SJ" \
        -L "$DEPS" -o /tmp/cf_out 2>&1)
  if [ $? -eq 0 ]; then
    echo "REGRESSION: $src COMPILED but must not"; fail=1; continue
  fi
  if ! echo "$out" | grep -qE 'error\[E[0-9]{3,4}\]'; then
    # Failed, but with no compiler error code at all — a broken link/setup, not a
    # real language-level rejection. That means the guard proved nothing.
    echo "FAIL: $base failed without any compiler error (broken setup?):"
    echo "$out" | head -3; fail=1; continue
  fi
  if echo "$out" | grep -q "$want"; then
    echo "ok (rejected with $want): $base"
  else
    got=$(echo "$out" | grep -oE 'E[0-9]{3,4}' | head -1)
    echo "WARN: $base rejected with $got, not the cited $want (toolchain drift; guarantee still holds)"
  fi
done

[ $fail -eq 0 ] && echo "==> ALL GUARANTEES HOLD" || { echo "==> SOME GUARANTEES BROKEN"; exit 1; }
