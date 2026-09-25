import { defineConfig } from "vite";
import { resolve } from "node:path";
import react from "@vitejs/plugin-react-swc";
import tailwindcss from "@tailwindcss/vite";
import { lingui } from "@lingui/vite-plugin";
import { LEGACY_BROWSER_TARGET, legacyCss } from "./vite/legacyBrowsers";

const packageRoot = import.meta.dirname;
const collaborateRoot = resolve(packageRoot, "../frontend/collaborate");

/** Build configuration for the host-owned collaborative application. */
export default defineConfig({
  root: collaborateRoot,
  base: "/collaborate/",
  resolve: {
    alias: [
      { find: /^neo-cucumber$/, replacement: resolve(packageRoot, "src/public.ts") },
      { find: /^neo-cucumber\/style\.css$/, replacement: resolve(packageRoot, "src/App.css") },
      { find: "react", replacement: resolve(packageRoot, "node_modules/react") },
      { find: "react-dom", replacement: resolve(packageRoot, "node_modules/react-dom") },
      { find: "@lingui/core", replacement: resolve(packageRoot, "node_modules/@lingui/core") },
      { find: "@lingui/react", replacement: resolve(packageRoot, "node_modules/@lingui/react") },
      { find: "@sentry/react", replacement: resolve(packageRoot, "node_modules/@sentry/react") },
      { find: "@iconify/react", replacement: resolve(packageRoot, "node_modules/@iconify/react") },
    ],
  },
  server: { allowedHosts: true },
  plugins: [
    react({ plugins: [["@lingui/swc-plugin", {}]] }),
    tailwindcss(),
    lingui(),
    legacyCss(),
  ],
  build: {
    // Firefox 56 syntax, for Waterfox Classic; see vite/legacyBrowsers.ts.
    target: LEGACY_BROWSER_TARGET,
    outDir: resolve(packageRoot, "dist"),
    emptyOutDir: true,
    // A map beside each file, for Sentry. The maps never reach the image's
    // served directories: the Dockerfile injects debug ids, sets them aside
    // for deploy.py to upload, and deletes them from what is served.
    sourcemap: true,
    rollupOptions: {
      input: {
        index: resolve(collaborateRoot, "index.html"),
      },
    },
  },
});
