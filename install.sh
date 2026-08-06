#!/usr/bin/env sh
# Install `kitec` and `kite-lsp`.
#
#   curl -fsSL https://kite-lang.dev/install.sh | sh
#
# It downloads one archive, checks it against the release's own checksum file,
# and unpacks two binaries. Nothing is compiled, nothing is run from the
# archive, and a checksum that does not match stops the install rather than
# warning about it.

set -eu

REPO="${KITE_REPO:-channyeintun/kite-lang}"
VERSION="${KITE_VERSION:-latest}"
PREFIX="${KITE_PREFIX:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "this needs \`$1\` on PATH"; }
need uname
need tar

case "$(uname -s)" in
  Darwin) os=apple-darwin ;;
  Linux) os=unknown-linux-musl ;;
  *) die "no prebuilt binary for $(uname -s); build from source with \`cargo build --release\`" ;;
esac

case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) die "no prebuilt binary for $(uname -m)" ;;
esac

target="$arch-$os"

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO "$2" "$1"; }
else
  die "this needs \`curl\` or \`wget\`"
fi

if [ "$VERSION" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# The archive's name carries the version, which `latest` does not know. The
# checksum file lists every archive in the release, so it is the thing to read
# first.
say "reading the release's checksums…"
# A 404 here almost always means there is no release yet rather than a network
# problem, and "cannot reach" sent people to check their connection. `curl`'s
# own message is hidden because it is the exit code that is interesting, not
# the transfer.
if ! fetch "$base/SHA256SUMS" "$work/SHA256SUMS" 2>/dev/null; then
  if [ "$VERSION" = "latest" ]; then
    die "$REPO has published no release yet, so there is nothing to install.

  Kite is pre-1.0. Build it from source instead — it needs Rust 1.85 and
  nothing else:

      git clone https://github.com/$REPO
      cd ${REPO#*/} && cargo build --release"
  fi
  die "no release \`$VERSION\` in $REPO"
fi

# The checksum file is signed with Sigstore, and the signature is checked when
# `cosign` is already installed. It is deliberately *not* installed here: a
# verifier fetched by the thing it is meant to verify proves nothing, so this
# says what to install and carries on rather than pretending to have checked.
if command -v cosign >/dev/null 2>&1; then
  say "checking the release's signature…"
  fetch "$base/SHA256SUMS.sigstore.json" "$work/SHA256SUMS.sigstore.json" ||
    die "this release has no signature; if you expected one, do not install it"
  cosign verify-blob "$work/SHA256SUMS" \
    --bundle "$work/SHA256SUMS.sigstore.json" \
    --certificate-identity-regexp "https://github\.com/$REPO/" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    >/dev/null 2>&1 || die "the signature over SHA256SUMS did not verify — refusing to install"
  say "  signed by the release workflow of $REPO"
else
  say "  (install \`cosign\` to check the release's signature as well)"
fi

archive="$(awk -v t="$target" '$2 ~ t { print $2 }' "$work/SHA256SUMS" | head -n 1)"
[ -n "$archive" ] || die "this release has no binary for $target"

say "downloading $archive…"
fetch "$base/$archive" "$work/$archive"

say "checking it…"
expected="$(awk -v a="$archive" '$2 == a { print $1 }' "$work/SHA256SUMS")"
if command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$work/$archive" | cut -d' ' -f1)"
elif command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$work/$archive" | cut -d' ' -f1)"
else
  die "this needs \`shasum\` or \`sha256sum\` to verify the download"
fi
[ "$expected" = "$actual" ] || die "checksum mismatch — refusing to install
  expected $expected
  found    $actual"

tar xzf "$work/$archive" -C "$work"
mkdir -p "$PREFIX"
for binary in kitec kite-lsp; do
  found="$(find "$work" -name "$binary" -type f | head -n 1)"
  [ -n "$found" ] || die "the archive has no \`$binary\`"
  install -m 755 "$found" "$PREFIX/$binary" 2>/dev/null || {
    cp "$found" "$PREFIX/$binary"
    chmod 755 "$PREFIX/$binary"
  }
  say "installed $PREFIX/$binary"
done

case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *) say ""
     say "$PREFIX is not on your PATH. Add it:"
     say "    export PATH=\"$PREFIX:\$PATH\"" ;;
esac

say ""
say "$("$PREFIX/kitec" --version)"
say "try:  kitec run hello.kite"
