#!/usr/bin/env bash
# Builds the Bitcoin V-App to wasm and runs wasm-bindgen. The app crate itself is the wasm
# module — there is no separate crate.
#
# Usage:  ./web/build.sh            # debug
#         ./web/build.sh --release  # smaller/faster wasm (recommended for a hosted demo)
set -euo pipefail
cd "$(dirname "$0")/.."   # apps/bitcoin/app

# wasm-bindgen-cli must match the `wasm-bindgen` crate version exactly.
WB_VERSION=0.2.105
have=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}' || true)
if [ "$have" != "$WB_VERSION" ]; then
  echo "error: wasm-bindgen-cli is '${have:-not installed}', need exactly $WB_VERSION." >&2
  echo "       cargo install wasm-bindgen-cli --version $WB_VERSION" >&2
  exit 1
fi

profile_dir=debug
extra=()
for arg in "$@"; do
  [ "$arg" = "--release" ] && profile_dir=release
  extra+=("$arg")
done

cargo rustc --lib --crate-type cdylib --target wasm32-unknown-unknown \
  --no-default-features --features target_wasm "${extra[@]}"

wasm="target/wasm32-unknown-unknown/${profile_dir}/vnd_bitcoin.wasm"
wasm-bindgen --target web    --out-dir web/pkg      "$wasm"   # browser demo
wasm-bindgen --target nodejs --out-dir web/pkg-node "$wasm"   # node test

# Optional size pass if binaryen's wasm-opt is on PATH (worthwhile for --release).
if command -v wasm-opt >/dev/null 2>&1; then
  for d in web/pkg web/pkg-node; do
    wasm-opt -Oz "$d/vnd_bitcoin_bg.wasm" -o "$d/vnd_bitcoin_bg.wasm"
  done
  echo "optimized with wasm-opt -Oz"
fi

echo "built: web/pkg (browser) and web/pkg-node (node test) from $wasm"
