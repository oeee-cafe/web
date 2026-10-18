import { defineConfig } from "vite";
import { LEGACY_BROWSER_TARGET } from "./vite/legacyBrowsers";
import { containScripts } from "./vite/iifeScope";
import { resolve } from "node:path";

/**
 * The site's own scripts (../frontend/site), for the server-rendered pages:
 * chrome.js, everything the pages run, loaded once a document by
 * theme_head.jinja; htmx with its extensions; and the reaction picker, a
 * module of its own so a browser without modules keeps the typed form. Kept
 * apart from the painter's build so that none of them pays for React.
 *
 * Emits stable filenames, served at /static/site/; the templates cache-bust
 * them themselves.
 */
export default defineConfig({
  // Served as text/javascript with no charset, so the minifier's UTF-8 would
  // reach a classic script read in the page's encoding. Escaped, the
  // Korean picker labels and the autocomplete's punctuation ranges are
  // plain ASCII whatever it guesses.
  esbuild: { charset: "ascii" },
  resolve: {
    // Nothing under ../frontend has a node_modules of its own to resolve
    // through, as the other configs say of theirs.
    alias: [
      {
        find: /^htmx\.org\//,
        replacement: resolve(import.meta.dirname, "node_modules/htmx.org") + "/",
      },
    ],
  },
  build: {
    // Firefox 56 syntax, for Waterfox Classic; see vite/legacyBrowsers.ts.
    target: LEGACY_BROWSER_TARGET,
    outDir: "dist-site",
    emptyOutDir: true,
    lib: {
      entry: {
        htmx: resolve(import.meta.dirname, "../frontend/site/htmx.ts"),
        // Everything else the pages run, loaded once a document by
        // theme_head.jinja: the app bridge, the chrome around every page,
        // and each page's own pieces (chrome.ts).
        chrome: resolve(import.meta.dirname, "../frontend/site/chrome.ts"),
        // Loaded as a module, so a browser without modules keeps the typed
        // form instead (reactionPicker.ts).
        reaction_picker: resolve(import.meta.dirname, "../frontend/site/reactionPicker.ts"),
      },
      formats: ["es"],
      fileName: (_format, name) => `${name}.js`,
    },
  },
  plugins: [containScripts()],
});
