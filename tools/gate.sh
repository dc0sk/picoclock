#!/usr/bin/env bash
# The local gate. Run before every commit. Fails on the first problem.
#   1. unit tests of the core crate (output is kept: the traceability check needs to see the bound tests ran)
#   2. traceability: requirement -> capability -> code -> test/record, and the generated documents are current
#   3. document headers
#   4. both firmware builds (skip with GATE_FAST=1), and the UF2 files made from them carry the right chip family
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
  echo "== UF2 family IDs"
  if command -v elf2uf2-rs >/dev/null; then
    UF2="$(mktemp -d)"
    elf2uf2-rs target/thumbv6m-none-eabi/release/picoclock "$UF2/w.uf2" >/dev/null
    elf2uf2-rs target/thumbv8m.main-none-eabihf/release/picoclock "$UF2/w2.uf2" >/dev/null
    python3 tools/uf2_family.py check "$UF2/w.uf2" rp2040
    # elf2uf2-rs stamps the RP2040 family on everything; the RP2350 boot ROM ignores such a file without saying so
    ! python3 tools/uf2_family.py check "$UF2/w2.uf2" rp2350-arm-s >/dev/null || { echo "elf2uf2-rs now writes the RP2350 family itself: drop the patch step from the README"; exit 1; }
    python3 tools/uf2_family.py set "$UF2/w2.uf2" "$UF2/w2s.uf2" rp2350-arm-s
    python3 tools/uf2_family.py check "$UF2/w2s.uf2" rp2350-arm-s
    rm -rf "$UF2"
  else
    echo "elf2uf2-rs not installed: UF2 family check skipped"
  fi
fi
echo "gate: ok"
