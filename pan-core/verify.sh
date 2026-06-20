#!/usr/bin/env bash
# Full verification for the Pan Wave 0 core:
#   1. build + unit/integration tests (the positive guarantees)
#   2. the compile-fail guards (the negative guarantees: bypasses must NOT compile)
#
# A core boundary regresses the moment a compile-fail guard starts compiling.
set -uo pipefail
cd "$(dirname "$0")"

echo "==> cargo test"
cargo test --quiet || { echo "FAIL: tests"; exit 1; }

echo "==> building rlib for compile-fail checks"
cargo build --quiet || { echo "FAIL: build"; exit 1; }
SJ=$(ls target/debug/deps/libserde_json-*.rlib | head -1)

fail=0
for src in tests/compile-fail/*.rs; do
  want=$(grep -oE 'E[0-9]{3,4}' "$src" | head -1)
  out=$(rustc --edition 2021 "$src" \
        --extern pan_core=target/debug/libpan_core.rlib \
        --extern serde_json="$SJ" \
        -L target/debug/deps -o /tmp/cf_out 2>&1)
  if [ $? -eq 0 ]; then
    echo "REGRESSION: $src COMPILED but must not"; fail=1; continue
  fi
  if echo "$out" | grep -q "$want"; then
    echo "ok (rejected with $want): $(basename "$src")"
  else
    echo "FAIL: $src failed but not with expected $want:"; echo "$out" | head -3; fail=1
  fi
done

[ $fail -eq 0 ] && echo "==> ALL GUARANTEES HOLD" || { echo "==> SOME GUARANTEES BROKEN"; exit 1; }
