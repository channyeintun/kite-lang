import kite from "vite-plugin-kite";

// That is the whole configuration. The plugin runs `kitec` when a `.kite`
// file is imported and hands Vite the module it produced — there is no
// runtime, nothing injected into the page, and no opinion about how the rest
// of the project is arranged.
export default {
  plugins: [kite()],
};
