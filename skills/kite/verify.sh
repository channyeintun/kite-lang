#!/usr/bin/env bash
# Compile every Kite example in a Markdown file.
#
# The point of this skill is that an agent stops guessing at Kite, so an
# example in it that does not compile is worse than no example: it teaches the
# guess. Every ```kite block here is a complete program and is compiled.
#
#   ```kite            a program that must compile
#   ```kite ignore     an illustration — a fragment, or a call into something
#                      that is not defined here. Not compiled, same convention
#                      the standard library's own doc comments use.
#   ```kite fails      a program that must NOT compile. The line that should be
#                      rejected carries `//~ E0302`, as `tests/corpus` does,
#                      and the compiler must report that code: a header
#                      `error[E0302]` or `warning[E0302]`. The code appearing
#                      anywhere else is not enough — a diagnostic quotes its
#                      source line, and that line carries the marker itself.
#
# Usage: verify.sh <file.md>...    exit 0 when every block behaved as marked.

set -uo pipefail

KITEC="${KITEC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/target/release/kitec}"
if [ ! -x "$KITEC" ]; then
  echo "no kitec at $KITEC — build it with: cargo build --release -p kitec" >&2
  exit 2
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
total=0
bad=0

for doc in "$@"; do
  # Split the file into fenced blocks with awk, one file per block, recording
  # the fence's info string and the line it opened on.
  awk -v out="$work" -v doc="$doc" '
    /^```kite( .*)?$/ && !inblock {
      inblock = 1; n++
      info = substr($0, 4)
      sub(/^kite[ ]*/, "", info)
      path = sprintf("%s/%03d.kite", out, n)
      printf "%s\t%s\t%d\n", path, (info == "" ? "compile" : info), NR >> (out "/index.tsv")
      next
    }
    /^```$/ && inblock { inblock = 0; close(path); next }
    inblock { print > path }
  ' "$doc"

  [ -f "$work/index.tsv" ] || { echo "  $doc: no kite blocks"; continue; }

  while IFS=$'\t' read -r path kind line; do
    case "$kind" in
      ignore) continue ;;
    esac
    total=$((total + 1))
    said="$("$KITEC" check "$path" 2>&1)"
    ok=$?
    if [ "$kind" = "fails" ]; then
      if [ $ok -eq 0 ]; then
        echo "FAIL $doc:$line — marked \`fails\` but it compiled"
        bad=$((bad + 1))
        continue
      fi
      # Every `//~ E0nnn` marker in the block must name a code the compiler
      # reported. Matching the bare code would pass on the echoed source line.
      while read -r code; do
        case "$said" in
          *"error[$code]"*|*"warning[$code]"*) ;;
          *) echo "FAIL $doc:$line — expected $code, got: $(echo "$said" | head -1)"
             bad=$((bad + 1)) ;;
        esac
      done < <(grep -o "//~ *E0[0-9][0-9][0-9]" "$path" | grep -o "E0[0-9][0-9][0-9]" | sort -u)
    else
      if [ $ok -ne 0 ]; then
        echo "FAIL $doc:$line — does not compile:"
        echo "$said" | sed 's/^/      /' | head -8
        bad=$((bad + 1))
      fi
    fi
  done < "$work/index.tsv"
  rm -f "$work/index.tsv"
done

echo "$total blocks compiled, $bad wrong"
[ "$bad" -eq 0 ]
