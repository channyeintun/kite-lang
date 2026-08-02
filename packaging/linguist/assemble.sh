#!/usr/bin/env bash
# Place Kite's samples into a Linguist checkout.
#
#   ./packaging/linguist/assemble.sh ../linguist
#
# The samples are copied from the library and the examples at the moment of
# submission rather than kept as a second copy here. A sample that drifted from
# the language it is a sample of is worse than none — it is what the classifier
# learns from — and this project already treats a copy nobody checks as a copy
# that rots.
#
# Linguist asks for "examples of real-world code showing common usage", and
# says outright that "'Hello world' and other examples found in tutorials will
# not be accepted". So `hello.kite` is deliberately absent, and what goes
# instead is the standard library, which is the largest body of Kite there is
# and was written to be read.
set -euo pipefail

linguist="${1:-}"
if [ -z "$linguist" ]; then
  echo "usage: assemble.sh <path-to-linguist-checkout>" >&2
  exit 2
fi
if [ ! -f "$linguist/lib/linguist/languages.yml" ]; then
  echo "error: $linguist does not look like a Linguist checkout" >&2
  exit 1
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$linguist/samples/Kite"
mkdir -p "$out"

# Chosen for breadth rather than length: a parser, a layout engine, an
# application, a module with a declared host boundary, and a program that uses
# the type system hard. Between them they reach most of the grammar.
samples=(
  "std/json.kite"        # recursion, enums, matching, error propagation
  "std/ui.kite"          # structs, slices, the largest thing here
  "std/http.kite"        # async, a declared host boundary, generics
  "examples/todo.kite"   # an application: init, view, update, widgets
  "examples/derive.kite" # attributes, traits, generics, interpolation
  "examples/traits.kite" # traits and dynamic dispatch on their own
)

for s in "${samples[@]}"; do
  cp "$root/$s" "$out/$(basename "$s")"
  echo "  samples/Kite/$(basename "$s")"
done

echo
echo "copied ${#samples[@]} samples into $out"
echo "next: paste the two fragments beside this script, then run script/update-ids"
