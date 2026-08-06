import { resolve } from "node:path";
import kite from "vite-plugin-kite";

// Two pages, each running its own program. Nothing about `index.html` or
// `main.kite` is special: the plugin wires whatever a `<script type="module">`
// points at, in whatever HTML Vite is given.
export default {
  plugins: [kite()],
  build: {
    rollupOptions: {
      input: {
        index: resolve(import.meta.dirname, "index.html"),
        about: resolve(import.meta.dirname, "about.html"),
      },
    },
  },
};
