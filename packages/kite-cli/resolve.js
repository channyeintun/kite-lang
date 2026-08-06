// Where the compiler is.
//
// One package per platform, each holding the real `kitec` for it, and npm
// installs only the one that matches through `os` and `cpu` in its manifest.
// That is how esbuild and swc do it, and the reason is the same: a binary
// fetched by a postinstall script is a supply-chain step that runs code on
// every install, and this needs none.
//
// **It is the compiler, not a copy of it.** Nothing here reimplements `fmt` or
// `check` — the whole point is that a project gets the same binary a terminal
// has, so the two cannot answer differently.

import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

/// The package for the platform this is running on.
const PACKAGES = {
  "darwin arm64": "@kite-lang/cli-darwin-arm64",
  "darwin x64": "@kite-lang/cli-darwin-x64",
  "linux arm64": "@kite-lang/cli-linux-arm64",
  "linux x64": "@kite-lang/cli-linux-x64",
  "win32 x64": "@kite-lang/cli-win32-x64",
};

/// The path to a binary this package provides, or null.
///
/// Null rather than a throw: a caller that can fall back to `PATH` should, and
/// one that cannot says so in its own words.
export function binary(name) {
  const key = `${process.platform} ${process.arch}`;
  const pkg = PACKAGES[key];
  if (!pkg) return null;
  const file = process.platform === "win32" ? `${name}.exe` : name;
  try {
    return require.resolve(`${pkg}/bin/${file}`);
  } catch {
    return null;
  }
}

/// What to tell someone when there is no binary for them.
export function unavailable(name) {
  const key = `${process.platform} ${process.arch}`;
  return PACKAGES[key]
    ? `kite-cli: ${PACKAGES[key]} is not installed.\n` +
        "npm skips an optional dependency that failed to install; try `npm install` again,\n" +
        `or install Kite directly — https://kite-lang.dev/install`
    : `kite-cli: no ${name} for ${key} yet.\n` +
        "Build from source — https://kite-lang.dev/install";
}
