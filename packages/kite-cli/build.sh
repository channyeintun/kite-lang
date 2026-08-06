#!/usr/bin/env sh
# Build the per-platform packages this one depends on.
#
# Each holds the real `kitec` and `kite-lsp` for one platform, with `os` and
# `cpu` set so npm installs exactly the one that matches and skips the rest.
# There is no postinstall script and nothing is downloaded at install time.
#
#   ./build.sh                 the platform you are on, from target/release
#   ./build.sh <dir>           every archive in a release directory
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$here/platforms"
version="$(node -p "require('$here/package.json').version")"

emit() {
  name="$1"; os="$2"; cpu="$3"; from="$4"; ext="${5:-}"
  dir="$out/cli-$name"
  mkdir -p "$dir/bin"
  cp "$from/kitec$ext" "$dir/bin/" 2>/dev/null || { echo "  skipped $name (no binary)"; return 0; }
  cp "$from/kite-lsp$ext" "$dir/bin/" 2>/dev/null || true
  cat > "$dir/package.json" <<JSON
{
  "name": "@kite-lang/cli-$name",
  "version": "$version",
  "description": "The Kite compiler for $os $cpu.",
  "license": "MIT",
  "repository": { "type": "git", "url": "git+https://github.com/channyeintun/kite-lang.git" },
  "os": ["$os"],
  "cpu": ["$cpu"],
  "files": ["bin"],
  "publishConfig": { "access": "public" }
}
JSON
  echo "  @kite-lang/cli-$name ($(du -h "$dir/bin/kitec$ext" | cut -f1))"
}

mkdir -p "$out"
if [ "${1:-}" = "--cross" ] || [ "${CROSS:-}" = "1" ]; then
  CROSS_MODE=1
elif [ $# -ge 1 ]; then
  echo "packaging from $1…"
  for archive in "$1"/*.tar.gz "$1"/*.zip; do
    [ -e "$archive" ] || continue
    work="$(mktemp -d)"
    case "$archive" in
      *.zip) unzip -q "$archive" -d "$work" ;;
      *) tar xzf "$archive" -C "$work" ;;
    esac
    from="$(find "$work" -name kitec -o -name kitec.exe | head -n 1 | xargs dirname)"
    case "$archive" in
      *aarch64-apple-darwin*) emit darwin-arm64 darwin arm64 "$from" ;;
      *x86_64-apple-darwin*) emit darwin-x64 darwin x64 "$from" ;;
      *aarch64-unknown-linux*) emit linux-arm64 linux arm64 "$from" ;;
      *x86_64-unknown-linux*) emit linux-x64 linux x64 "$from" ;;
      *x86_64-pc-windows*) emit win32-x64 win32 x64 "$from" .exe ;;
    esac
    rm -rf "$work"
  done
fi
if [ "${CROSS_MODE:-}" = "1" ]; then
  # Every target from this machine. Rust cross-compiles and this build has no C
  # dependency, so the only thing needed per target is a linker: Apple's own for
  # both macOS arches, and zig for musl and mingw.
  #
  #   rustup target add x86_64-apple-darwin x86_64-unknown-linux-musl \
  #                     aarch64-unknown-linux-musl x86_64-pc-windows-gnu
  #   cargo install cargo-zigbuild
  #
  # **These are not what a release ships.** CI builds each on its own runner and
  # signs the checksums, and only that build is reproducible and verifiable.
  # This is for trying the packaging without waiting for a tag.
  echo "cross-building every target…"
  ( cd "$root" && cargo build --release -p kitec -p kite-lsp )
  ( cd "$root" && cargo build --release -p kitec -p kite-lsp --target x86_64-apple-darwin )
  for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-gnu; do
    ( cd "$root" && cargo zigbuild --release -p kitec -p kite-lsp --target "$t" )
  done
  emit darwin-arm64 darwin arm64 "$root/target/release"
  emit darwin-x64 darwin x64 "$root/target/x86_64-apple-darwin/release"
  emit linux-x64 linux x64 "$root/target/x86_64-unknown-linux-musl/release"
  emit linux-arm64 linux arm64 "$root/target/aarch64-unknown-linux-musl/release"
  emit win32-x64 win32 x64 "$root/target/x86_64-pc-windows-gnu/release" .exe
elif [ $# -eq 0 ]; then
  echo "packaging the platform you are on, from target/release…"
  cargo build --release -p kitec -p kite-lsp --manifest-path "$root/Cargo.toml" >/dev/null
  case "$(uname -s) $(uname -m)" in
    "Darwin arm64") emit darwin-arm64 darwin arm64 "$root/target/release" ;;
    "Darwin x86_64") emit darwin-x64 darwin x64 "$root/target/release" ;;
    "Linux aarch64") emit linux-arm64 linux arm64 "$root/target/release" ;;
    "Linux x86_64") emit linux-x64 linux x64 "$root/target/release" ;;
    *) echo "  no mapping for $(uname -s) $(uname -m)"; exit 1 ;;
  esac
fi
echo
echo "publish the platform packages first, then the meta-package:"
echo "  for d in $out/*/; do (cd \"\$d\" && npm publish); done"
echo "  npm publish $here"
echo
echo "Each manifest carries \`publishConfig.access = public\`. A scoped package"
echo "defaults to private, and npm answers that with 402 Payment Required —"
echo "which reads as a billing problem and is a missing flag."
