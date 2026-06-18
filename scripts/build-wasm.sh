#!/usr/bin/env bash
# Build the doc-viewer WASM core and generate the JS glue into the npm package.
#
#   ./scripts/build-wasm.sh            # release build
#   ./scripts/build-wasm.sh --dev      # faster, unoptimized build
#
# Requires: rustup target wasm32-unknown-unknown, wasm-bindgen-cli (matching the
# wasm-bindgen crate version). wasm-opt (binaryen) is used if present.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/packages/doc-viewer/wasm"
PROFILE_DIR="release"
CARGO_FLAGS="--release"

if [[ "${1:-}" == "--dev" ]]; then
  PROFILE_DIR="debug"
  CARGO_FLAGS=""
fi

echo "==> cargo build (wasm32, $PROFILE_DIR)"
cargo build -p dv-wasm --target wasm32-unknown-unknown $CARGO_FLAGS

WASM="$ROOT/target/wasm32-unknown-unknown/$PROFILE_DIR/dv_wasm.wasm"

echo "==> wasm-bindgen -> $OUT"
wasm-bindgen "$WASM" --out-dir "$OUT" --target web

if command -v wasm-opt >/dev/null 2>&1; then
  echo "==> wasm-opt -Oz (-all: wasm-bindgen emits post-MVP features)"
  wasm-opt -Oz -all "$OUT/dv_wasm_bg.wasm" -o "$OUT/dv_wasm_bg.wasm.opt"
  mv "$OUT/dv_wasm_bg.wasm.opt" "$OUT/dv_wasm_bg.wasm"
else
  echo "==> wasm-opt not found (skipping size pass; install binaryen to shrink the .wasm)"
fi

echo "==> done. core size:"
ls -lh "$OUT/dv_wasm_bg.wasm"
