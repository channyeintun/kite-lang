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

echo "rendering the pages…"
# Every document this site serves is known now, so it is rendered now — by the
# same Kite renderer a browser would have run, executed here instead. The pages
# it writes need no JavaScript and show no `Loading…`.
mkdir -p "$out/read"
(cd "$root" && "$root/target/release/kitec" run site/src/build.kite)

echo "building the site's own program…"
# The site is a Kite program. The Markdown rendering, the syntax colouring, the
# navigation and the fetching are all in `site/src/`, compiled here to the
# `app.wasm` every page instantiates. What is left in JavaScript on those pages
# is the four lines that do the instantiating.
"$root/target/release/kitec" build "$here/src/main.kite" --emit wasm --out "$out"
echo "  app.wasm ($(wc -c < "$out/app.wasm" | tr -d " ") bytes)"

echo "building the demo…"
# The page, the stylesheet and the program are copied; the module is compiled
# beside them. What the site serves is exactly what `kitec build` produces
# against a page nobody generated — which is the whole claim, so demonstrating
# it any other way would be demonstrating something else.
#
# `kitec build` leaves `index.html` alone when one is already there, so the
# order here matters: copy the page first.
mkdir -p "$out/demo"
cp "$root/examples/page/index.html" "$out/demo/index.html"
cp "$root/examples/page/style.css" "$out/demo/style.css"
"$root/target/release/kitec" build "$root/examples/page/main.kite" --emit wasm --out "$out/demo"
echo "  demo/index.html ($(wc -c < "$out/demo/app.wasm" | tr -d " ") bytes of WebAssembly)"

echo "copying the documents…"
mkdir -p "$out/docs"
cp "$root/SPECIFICATION.md" "$out/SPECIFICATION.md"
cp "$root/README.md" "$out/README.md"
cp "$root"/docs/*.md "$out/docs/"
# The installer is served from the site, because that is the URL its own first
# line tells people to pipe into a shell. It was not copied, so the documented
# one-liner fetched a 404.
cp "$root/install.sh" "$out/install.sh"

echo
echo "done. serve it with any static server:"
echo "    python3 -m http.server -d $out 8000"
