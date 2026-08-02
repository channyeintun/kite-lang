#!/usr/bin/env bash
# Fill the three package manifests in from a release's own checksums.
#
#   ./packaging/render.sh v0.2.0            # from the published release
#   ./packaging/render.sh v0.2.0 dist/      # from a local dist/SHA256SUMS
#
# The checksums come from `SHA256SUMS`, which is the file the release signs.
# Nothing here computes a hash from a download of its own: a manifest whose
# checksum was derived from the same fetch it is meant to protect proves
# nothing, and one typed by hand proves less.
#
# Written into `packaging/out/`, which is not committed — the manifests in the
# tree keep their placeholders so they stay reviewable, and the real ones are
# release artefacts.
set -euo pipefail

version="${1:-}"
[ -n "$version" ] || { echo "usage: render.sh <tag> [dist-directory]" >&2; exit 2; }
bare="${version#v}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="$here/out"
mkdir -p "$out"

sums="$out/SHA256SUMS"
if [ -n "${2:-}" ]; then
  cp "$2/SHA256SUMS" "$sums"
else
  repo="${KITE_REPO:-channyeintun/kite-lang}"
  curl -fsSL "https://github.com/$repo/releases/download/$version/SHA256SUMS" -o "$sums"
fi

# The archive names carry the target, so one grep per target is the whole of it.
hash_for() {
  local suffix="$1"
  local line
  line="$(grep -E "  kite-$version-$suffix\$" "$sums" || true)"
  if [ -z "$line" ]; then
    echo "error: $sums has no entry for kite-$version-$suffix" >&2
    exit 1
  fi
  printf '%s' "${line%% *}"
}

mac_arm="$(hash_for aarch64-apple-darwin.tar.gz)"
mac_intel="$(hash_for x86_64-apple-darwin.tar.gz)"
linux_arm="$(hash_for aarch64-unknown-linux-musl.tar.gz)"
linux_intel="$(hash_for x86_64-unknown-linux-musl.tar.gz)"
windows="$(hash_for x86_64-pc-windows-msvc.zip)"

# Homebrew: four archives, in the order the formula writes them.
awk -v v="$bare" -v a="$mac_arm" -v b="$mac_intel" -v c="$linux_arm" -v d="$linux_intel" '
  /^  version "/ { print "  version \"" v "\""; next }
  /^      sha256 "/ {
    n += 1
    if (n == 1) { print "      sha256 \"" a "\"" ; next }
    if (n == 2) { print "      sha256 \"" b "\"" ; next }
    if (n == 3) { print "      sha256 \"" c "\"" ; next }
    print "      sha256 \"" d "\""; next
  }
  { print }
' "$here/homebrew/kite.rb" > "$out/kite.rb"

sed -e "s/0\\.0\\.0/$bare/g" \
    -e "s/0000000000000000000000000000000000000000000000000000000000000000/$windows/" \
    "$here/scoop/kite.json" > "$out/kite.json"

sed -e "s/^pkgver=.*/pkgver=$bare/" \
    -e "s/^sha256sums_x86_64=.*/sha256sums_x86_64=('$linux_intel')/" \
    -e "s/^sha256sums_aarch64=.*/sha256sums_aarch64=('$linux_arm')/" \
    "$here/aur/PKGBUILD" > "$out/PKGBUILD"

echo "wrote:"
echo "  $out/kite.rb"
echo "  $out/kite.json"
echo "  $out/PKGBUILD"
