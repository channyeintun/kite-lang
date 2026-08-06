#!/usr/bin/env sh
# Rasterise the extension tile.
#
# `icon.svg` is the drawing; `icon.png` is a rendering of it, because the
# Marketplace will not take an SVG. Run this when the mark changes — the mark
# itself is decided in `site/kite-mark.svg`, and `brand_assets.rs` fails if
# this tile has drifted from it.
#
# Three renderers, because none of them is on every machine and the answer is
# the same from all of them: this drawing is two flat shapes on a rounded
# rectangle, with no filter, gradient or font in it.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"

if command -v rsvg-convert >/dev/null 2>&1; then
  rsvg-convert -w 256 -h 256 "$here/icon.svg" -o "$here/icon.png"
elif command -v magick >/dev/null 2>&1; then
  magick -background none -density 384 "$here/icon.svg" -resize 256x256 "$here/icon.png"
elif command -v inkscape >/dev/null 2>&1; then
  inkscape "$here/icon.svg" -w 256 -h 256 -o "$here/icon.png" >/dev/null
else
  echo "error: needs rsvg-convert, magick or inkscape to rasterise the tile" >&2
  exit 1
fi

echo "wrote $here/icon.png"
