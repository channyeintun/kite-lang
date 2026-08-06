#!/usr/bin/env sh
# Build the compiler for WebAssembly and put it in this package.
#
# There is no platform matrix here and nothing to cross-compile: one module
# serves every operating system, because WebAssembly is the target.
#
#   ./build.sh
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"

echo "building the compiler for WebAssembly…"
cargo build --release -p kite-playground --target wasm32-unknown-unknown --manifest-path "$root/Cargo.toml"
cp "$root/target/wasm32-unknown-unknown/release/kite_playground.wasm" "$here/kite-compiler.wasm"

# `wasm-opt` takes about 20% off and nothing breaks when it is not installed.
if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -Oz "$here/kite-compiler.wasm" -o "$here/kite-compiler.wasm"
else
  echo "  note: wasm-opt is not installed; shipping the module unoptimised"
fi

# `wc -c`, not `du`: `du` reports allocated blocks and reads ~20% high here,
# which would make the optimisation above look like it had done nothing.
echo "  $(node -p "($(wc -c < "$here/kite-compiler.wasm") / 1048576).toFixed(1)")M  kite-compiler.wasm"
