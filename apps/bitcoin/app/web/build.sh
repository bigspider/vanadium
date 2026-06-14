#!/usr/bin/env bash
# Builds the Bitcoin V-App to wasm and runs wasm-bindgen. The app crate itself is the wasm
# module — there is no separate crate. Requires `wasm-bindgen-cli` matching the `wasm-bindgen`
# crate version (currently 0.2.105): `cargo install wasm-bindgen-cli --version 0.2.105`.
#
# Usage:  ./web/build.sh            # debug
#         ./web/build.sh --release  # smaller/faster wasm
set -euo pipefail
cd "$(dirname "$0")/.."   # apps/bitcoin/app

profile_dir=debug
extra=()
for arg in "$@"; do
  if [ "$arg" = "--release" ]; then profile_dir=release; fi
  extra+=("$arg")
done

cargo rustc --lib --crate-type cdylib --target wasm32-unknown-unknown \
  --no-default-features --features target_wasm "${extra[@]}"

wasm="target/wasm32-unknown-unknown/${profile_dir}/vnd_bitcoin.wasm"
wasm-bindgen --target web    --out-dir web/pkg      "$wasm"   # browser demo
wasm-bindgen --target nodejs --out-dir web/pkg-node "$wasm"   # node test

echo "built: web/pkg (browser) and web/pkg-node (node test) from $wasm"
