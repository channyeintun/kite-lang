#!/usr/bin/env bash
# Build the site: the compiler as WebAssembly, the reference from the library's
# own source, and the documents the pages render.
#
# Everything here is a copy or a generation. There is no bundler, no framework
# and no dependency to fetch — a language's site should be readable from a
# directory and servable from anything.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
out="$here"

echo "building the compiler for WebAssembly…"
cargo build --release -p kite-playground --target wasm32-unknown-unknown --manifest-path "$root/Cargo.toml"
cp "$root/target/wasm32-unknown-unknown/release/kite_playground.wasm" "$out/kite_playground.wasm"
# `wasm-opt` halves it when it is available, and nothing breaks when it is not.
if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -Oz "$out/kite_playground.wasm" -o "$out/kite_playground.wasm"
fi
echo "  $(du -h "$out/kite_playground.wasm" | cut -f1)"

echo "generating the reference…"
mkdir -p "$out/reference"
cargo build --release -p kitec --manifest-path "$root/Cargo.toml"
for module in "$root"/std/*.kite; do
  name="$(basename "$module" .kite)"
  "$root/target/release/kitec" doc "$module" > "$out/reference/$name.md"
  echo "  reference/$name.md"
done

echo "building the demo…"
# `kitec build` writes the page as well as the module, and what the site serves
# is exactly that page — the compiler's own output, not a hand-written copy of
# it. A demonstration of what `kitec build` produces should *be* what `kitec
# build` produces, or it is a demonstration of something else.
mkdir -p "$out/demo"
"$root/target/release/kitec" build "$root/examples/boids.kite" --emit wasm --out "$out/demo"
echo "  demo/index.html"
mkdir -p "$out/player"
"$root/target/release/kitec" build "$root/examples/player.kite" --emit wasm --out "$out/player"
echo "  player/index.html"
# The recording the player names. Copied rather than referenced, because the
# module asks the host for a URL relative to the page it is served from.
#
# AAC in an MP4 container rather than MP3, and the container is the point. An
# MP3 is a bare run of frames with no index, so a player can only *estimate*
# where a timestamp lives and a variable-rate file defeats even that. MP4
# carries a sample table — an exact map from time to byte — and `+faststart`
# puts it at the front, so the whole index has arrived within the first fifty
# kilobytes. It is also half the size at the same audible quality.
cp "$root/assets/khin-maung-toe.m4a" "$out/player/khin-maung-toe.m4a"
echo "  player/khin-maung-toe.m4a"

echo "copying the documents…"
mkdir -p "$out/docs"
cp "$root/SPECIFICATION.md" "$out/SPECIFICATION.md"
cp "$root/README.md" "$out/README.md"
cp "$root"/docs/*.md "$out/docs/"

echo
echo "done. serve it with any static server:"
echo "    python3 -m http.server -d $out 8000"
