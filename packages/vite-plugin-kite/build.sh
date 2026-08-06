#!/usr/bin/env sh
# Give the starter its copy of the plugin.
#
# `examples/vite-starter` vendors this file rather than depending on the
# package, because the package is not published and a starter has to work when
# it is copied out of the repository. A test fails if the copy drifts, and this
# is how it is kept from drifting.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"

cp "$here/index.js" "$root/examples/vite-starter/plugin/vite-plugin-kite.js"
echo "copied index.js to the starter"
