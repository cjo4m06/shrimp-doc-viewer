#!/usr/bin/env bash
# Build the doc-viewer WASM core and generate the JS glue into the npm package.
#
#   ./scripts/build-wasm.sh            # release build
#   ./scripts/build-wasm.sh --dev      # faster, unoptimized build
#
# Requires: rustup target wasm32-unknown-unknown, wasm-bindgen-cli (matching the
# wasm-bindgen crate version).
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

# NOTE: the wasm-opt (binaryen) size pass is intentionally DISABLED. wasm-bindgen's
# output round-trips through binaryen as GC-style "rec group" types; binaryen 130
# either aborts (without --enable-gc: "distinct rec groups would be identical")
# or, with `-Oz -all`, re-encodes them in the GC binary form, which browsers and
# Node reject ("WebAssembly.instantiateStreaming: unknown type form") — i.e. it
# emits an INVALID module. This shipped a broken core in 0.1.0/0.1.1. The raw
# wasm-bindgen output is valid and only ~5% larger, so we ship that as-is; the CI
# publish job runs WebAssembly.validate as a backstop. Revisit only if a future
# binaryen optimizes this module to a *valid* result (validate before trusting it).

echo "==> done (no wasm-opt; raw wasm-bindgen output). core size:"
ls -lh "$OUT/dv_wasm_bg.wasm"
