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
# Every document under `docs/` except the roadmap, which the site no longer
# renders: it is a record of how the language was built, and the site is for
# the language as it is. It stays in the repository, where that history
# belongs.
for doc in "$root"/docs/*.md; do
  case "$(basename "$doc")" in
    06-roadmap.md) continue ;;
  esac
  cp "$doc" "$out/docs/"
done
# The installer is served from the site, because that is the URL its own first
# line tells people to pipe into a shell. It was not copied, so the documented
# one-liner fetched a 404.
cp "$root/install.sh" "$out/install.sh"
# The grammar is not a `.md`, and the loop above only takes those.
cp "$root/docs/05-grammar.ebnf" "$out/docs/05-grammar.ebnf"

echo "publishing the skill…"
# `llms.txt` points agents at the skill, so the skill has to be reachable. It
# is served as its own source rather than rendered: what wants it is a program,
# and every example in it is compiled by `verify.sh` as a condition of being
# written down.
rm -rf "$out/skill"
mkdir -p "$out/skill/references"
cp "$root/skills/kite/SKILL.md" "$out/skill/SKILL.md"
cp "$root"/skills/kite/references/*.md "$out/skill/references/"
# A directory listing is not served by a static asset host, and `llms.txt`
# links to `skill/references/`. One index, generated so it cannot go stale.
{
  echo "# Kite — skill reference pages"
  echo
  for page in "$root"/skills/kite/references/*.md; do
    name="$(basename "$page")"
    printf -- "- [%s](%s): %s\n" "$name" "$name" "$(sed -n 's/^# //p' "$page" | head -1)"
  done
} > "$out/skill/references/index.md"
echo "  skill/SKILL.md and $(ls "$root"/skills/kite/references/*.md | wc -l | tr -d " ") reference pages"

# The same for `docs/`, for the same reason: `llms.txt` cannot link a bare
# directory, because the asset server has no listing to serve.
{
  echo "# Kite — design notes"
  echo
  echo "How the language was decided, rather than what it is."
  echo
  for doc in "$out"/docs/*.md; do
    name="$(basename "$doc")"
    case "$name" in index.md) continue ;; esac
    printf -- "- [%s](%s): %s\n" "$name" "$name" "$(sed -n 's/^# //p' "$doc" | head -1)"
  done
  printf -- "- [05-grammar.ebnf](05-grammar.ebnf): the complete grammar\n"
} > "$out/docs/index.md"

echo "concatenating llms-full.txt…"
# One fetch for an agent that would rather not make seven. Generated, so it is
# the same bytes as the skill it was built from.
{
  echo "# Kite — the complete language reference for an agent"
  echo
  echo "Generated from skills/kite by site/build.sh. Every fenced \`kite\` example"
  echo "below is compiled by the real compiler; a \`kite fails\` example is one the"
  echo "compiler must reject, and the code it must be rejected with is on the"
  echo "offending line."
  echo
  cat "$root/skills/kite/SKILL.md"
  for page in "$root"/skills/kite/references/*.md; do
    echo
    echo "---"
    echo
    cat "$page"
  done
} > "$out/llms-full.txt"
echo "  llms-full.txt ($(wc -c < "$out/llms-full.txt" | tr -d " ") bytes)"

echo
echo "done. serve it with any static server:"
echo "    python3 -m http.server -d $out 8000"
