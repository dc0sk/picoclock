#!/usr/bin/env bash
# The local gate. Run before every commit. Fails on the first problem.
#   1. unit tests of the core crate (output is kept: the traceability check needs to see the bound tests ran)
#   2. traceability: requirement -> capability -> code -> test/record, and the generated documents are current
#   3. image signing tool self-test, document headers
#   4. both firmware builds (skip with GATE_FAST=1), and the UF2 files made from them carry the right chip family
set -euo pipefail
cd "$(dirname "$0")/.."
HOST="${HOST_TARGET:-$(rustc -vV | sed -n 's/^host: //p')}"
OUT="$(mktemp)"
trap 'rm -f "$OUT"' EXIT

echo "== unit tests ($HOST)"
cargo test -p picoclock-core --target "$HOST" 2>&1 | tee "$OUT" | grep -E "^test result|FAILED|panicked" || true
grep -q "^test result: ok" "$OUT" && ! grep -q "FAILED" "$OUT" || { echo "unit tests failed"; exit 1; }
# The Pico 2 W build verifies RSA chains (feature `rsa`); its tests run too, and count as run for the traceability check.
echo "== unit tests with RSA support ($HOST)"
cargo test -p picoclock-core --target "$HOST" --features rsa 2>&1 | tee -a "$OUT" | grep -E "^test result|FAILED|panicked" || true
! grep -q "FAILED" "$OUT" || { echo "unit tests with RSA support failed"; exit 1; }

echo "== traceability"
python3 tools/trace.py check --tests-output "$OUT"
python3 tools/trace.py self-test | tail -3

echo "== image signing tool"
python3 tools/sign_image.py self-test
python3 tools/combine_image.py check-layout

echo "== document headers"
python3 tools/check_docs.py
python3 tools/check_docs.py self-test | tail -1

if [ "${GATE_FAST:-0}" != "1" ]; then
  echo "== firmware: Pico W"
  cargo build --release -p picoclock
  echo "== firmware: Pico 2 W"
  cargo build --release -p picoclock --no-default-features --features rp235x --target thumbv8m.main-none-eabihf
  echo "== bootloaders"
  cargo build --release -p picoclock-bootloader
  cargo build --release -p picoclock-bootloader --no-default-features --features rp235x --target thumbv8m.main-none-eabihf
  echo "== combined images (bootloader + empty state sector + application)"
  UF2="$(mktemp -d)"
  python3 tools/combine_image.py target/thumbv6m-none-eabi/release/picoclock-bootloader target/thumbv6m-none-eabi/release/picoclock "$UF2/w.uf2" rp2040
  python3 tools/combine_image.py target/thumbv8m.main-none-eabihf/release/picoclock-bootloader target/thumbv8m.main-none-eabihf/release/picoclock "$UF2/w2.uf2" rp2350-arm-s
  python3 tools/uf2_family.py check "$UF2/w.uf2" rp2040
  python3 tools/uf2_family.py check "$UF2/w2.uf2" rp2350-arm-s
  rm -rf "$UF2"
  # the test build with a simulated GPS receiver must keep compiling (it overwrites the Pico 2 W artefact, so it comes last)
  echo "== firmware: Pico 2 W with the simulated GPS receiver (test build)"
  cargo build --release -p picoclock --no-default-features --features rp235x,gps-sim --target thumbv8m.main-none-eabihf
fi
echo "gate: ok"
