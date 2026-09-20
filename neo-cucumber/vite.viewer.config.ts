import { defineConfig } from "vite";
import react from "@vitejs/plugin-react-swc";
import { lingui } from "@lingui/vite-plugin";
import { LEGACY_BROWSER_TARGET, legacyCss } from "./vite/legacyBrowsers";
import path from "node:path";

/**
 * Builds the replay viewer as a standalone script for server-rendered pages,
 * the same way neo.js was consumed. Kept separate from the app build so the
 * viewer does not drag in React, Lingui, Sentry or the collaborative client --
 * a page that only plays a drawing should not pay for a painter.
 *
 * Emits a stable filename; the templates cache-bust it themselves.
 */
export default defineConfig({
  // Library mode leaves CommonJS environment guards in place, and this bundle
  // runs straight in a browser where `process` does not exist. Lingui's runtime
  // checks it; without this the viewer throws before it can mount.
  define: { "process.env.NODE_ENV": JSON.stringify("production") },
  // The SWC plugin is here for Lingui's macro rather than for React: the
  // viewer has no components, but its labels are messages like every other
  // string in this package and the macro is what turns them into catalog ids.
  plugins: [react({ plugins: [["@lingui/swc-plugin", {}]] }), lingui(), legacyCss()],
  build: {
    // Firefox 56 syntax, for Waterfox Classic; see vite/legacyBrowsers.ts.
    target: LEGACY_BROWSER_TARGET,
    outDir: "dist-viewer",
    emptyOutDir: true,
    lib: {
      entry: path.resolve(import.meta.dirname, "src/viewer/embed.ts"),
      name: "NeoCucumberReplay",
      formats: ["iife"],
      fileName: () => "neo-cucumber-replay.js",
    },
  },
});
