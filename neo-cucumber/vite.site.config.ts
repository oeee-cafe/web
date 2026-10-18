import { defineConfig } from "vite";
import { LEGACY_BROWSER_TARGET } from "./vite/legacyBrowsers";
import { containScripts } from "./vite/iifeScope";
import { resolve } from "node:path";

/**
 * The site's own small scripts (../frontend/site), each a file of its own that
 * a server-rendered page loads by a script tag: the comment box's @
 * autocomplete, a post's pinch to zoom, and the reaction picker. Kept apart
 * from the painter's build so that none of them pays for React.
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
  build: {
    // Firefox 56 syntax, for Waterfox Classic; see vite/legacyBrowsers.ts.
    target: LEGACY_BROWSER_TARGET,
    outDir: "dist-site",
    emptyOutDir: true,
    lib: {
      entry: {
        mention: resolve(import.meta.dirname, "../frontend/site/mention.ts"),
        post_zoom: resolve(import.meta.dirname, "../frontend/site/postZoom.ts"),
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
