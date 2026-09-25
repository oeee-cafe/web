import { defineConfig } from "vite";
import react from "@vitejs/plugin-react-swc";
import tailwindcss from "@tailwindcss/vite";
import { lingui } from "@lingui/vite-plugin";
import { LEGACY_BROWSER_TARGET, legacyCss } from "./vite/legacyBrowsers";
import { resolve } from "node:path";

export default defineConfig({
  // Library mode does not replace CommonJS environment guards automatically.
  // The offline bundle runs directly in browsers, where `process` does not
  // exist, so select production React/Lingui branches at build time.
  define: {
    "process.env.NODE_ENV": JSON.stringify("production"),
  },
  resolve: {
    // Exact patterns, not prefixes: a bare string alias for "neo-cucumber"
    // also rewrites "neo-cucumber/style.css" into a path that does not exist.
    alias: [
      {
        find: /^neo-cucumber$/,
        replacement: resolve(import.meta.dirname, "src/public.ts"),
      },
      {
        find: /^neo-cucumber\/style\.css$/,
        replacement: resolve(import.meta.dirname, "src/App.css"),
      },
      // The host under ../frontend has no node_modules of its own to resolve
      // through, as the other configs say of theirs.
      {
        find: "@sentry/browser",
        replacement: resolve(import.meta.dirname, "node_modules/@sentry/browser"),
      },
    ],
  },
  plugins: [
    react({ plugins: [["@lingui/swc-plugin", {}]] }),
    tailwindcss(),
    lingui(),
    legacyCss(),
  ],
  build: {
    // Firefox 56 syntax, for Waterfox Classic; see vite/legacyBrowsers.ts.
    target: LEGACY_BROWSER_TARGET,
    outDir: "dist-offline",
    emptyOutDir: true,
    // A map beside each file, for Sentry. The maps never reach the image's
    // served directories: the Dockerfile injects debug ids, sets them aside
    // for deploy.py to upload, and deletes them from what is served.
    sourcemap: true,
    lib: {
      // The painter, and the drafts page's list of the drawings it kept in
      // the browser (frontend/shared/localDrafts.ts), which the two share.
      entry: {
        offline: resolve(import.meta.dirname, "../frontend/painter/entry.ts"),
        drafts: resolve(import.meta.dirname, "../frontend/drafts/entry.ts"),
        // The painter page's error reporter, kept out of offline.js so that
        // an engine the SDK does not run on loses the reporter and not the
        // painter; see frontend/painter/sentry.ts.
        sentry: resolve(import.meta.dirname, "../frontend/painter/sentry.ts"),
      },
      formats: ["es"],
      fileName: (_format, name) => `${name}.js`,
    },
    cssCodeSplit: false,
    rollupOptions: {
      output: {
        assetFileNames: (asset) =>
          asset.names?.some((name) => name.endsWith(".css"))
            ? "offline.css"
            : "assets/[name]-[hash][extname]",
      },
    },
  },
});
