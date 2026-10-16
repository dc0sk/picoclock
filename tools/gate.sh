#!/usr/bin/env bash
# The local gate. Run before every commit. Fails on the first problem.
#   1. unit tests of the core crate (output is kept: the traceability check needs to see the bound tests ran)
#   2. traceability: requirement -> capability -> code -> test/record, and the generated documents are current
#   3. document headers
#   4. both firmware builds (skip with GATE_FAST=1)
set -euo pipefail
cd "$(dirname "$0")/.."
HOST="${HOST_TARGET:-$(rustc -vV | sed -n 's/^host: //p')}"
OUT="$(mktemp)"
trap 'rm -f "$OUT"' EXIT

echo "== unit tests ($HOST)"
cargo test -p picoclock-core --target "$HOST" 2>&1 | tee "$OUT" | grep -E "^test result|FAILED|panicked" || true
grep -q "^test result: ok" "$OUT" && ! grep -q "FAILED" "$OUT" || { echo "unit tests failed"; exit 1; }

echo "== traceability"
python3 tools/trace.py check --tests-output "$OUT"

echo "== document headers"
python3 tools/check_docs.py

if [ "${GATE_FAST:-0}" != "1" ]; then
  echo "== firmware: Pico W"
  cargo build --release -p picoclock
  echo "== firmware: Pico 2 W"
  cargo build --release -p picoclock --no-default-features --features rp235x --target thumbv8m.main-none-eabihf
fi
echo "gate: ok"
