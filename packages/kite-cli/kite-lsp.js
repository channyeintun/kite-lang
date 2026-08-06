#!/usr/bin/env node
// `kite-lsp`, forwarded.
//
// Every argument, both streams and the exit code pass straight through, so
// this is the compiler and not a wrapper with opinions. `npm run fmt` and a
// terminal run the same program.
import { spawnSync } from "node:child_process";
import { binary, unavailable } from "./resolve.js";

const bin = binary("kite-lsp");
if (!bin) {
  process.stderr.write(unavailable("kite-lsp") + "\n");
  process.exit(1);
}
process.exit(spawnSync(bin, process.argv.slice(2), { stdio: "inherit" }).status ?? 1);
