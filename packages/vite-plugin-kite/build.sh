#!/usr/bin/env sh
# Build the compiler this package ships, and give the starter its copy.
#
# `kitec.wasm` is the same Rust as the `kitec` binary, built for WebAssembly,
# so a project using this plugin installs nothing. `examples/vite-starter`
# carries its own copy because it has to keep working when it is copied out of
# the repository — a test fails if the two stop matching.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"

cargo build --release -p kite-playground --target wasm32-unknown-unknown --manifest-path "$root/Cargo.toml"
cp "$root/target/wasm32-unknown-unknown/release/kite_playground.wasm" "$here/kitec.wasm"

# `wasm-opt` takes about 20% off and nothing breaks when it is absent.
if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -Oz "$here/kitec.wasm" -o "$here/kitec.wasm"
fi

cp "$here/index.js" "$root/examples/vite-starter/plugin/vite-plugin-kite.js"
cp "$here/kitec.wasm" "$root/examples/vite-starter/plugin/kitec.wasm"
echo "kitec.wasm: $(wc -c < "$here/kitec.wasm" | tr -d ' ') bytes, copied to the starter"
