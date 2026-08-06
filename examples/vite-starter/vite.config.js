// The plugin is imported from `plugin/` rather than from `node_modules`,
// because it is not published to npm yet and a starter has to work when it is
// copied out of this repository — which is the only thing a starter is for.
//
// `plugin/vite-plugin-kite.js` is a byte-for-byte copy of
// `packages/vite-plugin-kite/index.js`, and a test in the compiler's suite
// fails if the two ever stop matching. When the package is published this
// becomes:
//
//     import kite from "vite-plugin-kite";
//
// with `"vite-plugin-kite": "^0.1.0"` in `devDependencies`, and `plugin/`
// goes away.
import kite from "./plugin/vite-plugin-kite.js";

export default {
  plugins: [kite()],
};
