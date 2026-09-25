import { defineConfig } from "vite";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react-swc";
import tailwindcss from "@tailwindcss/vite";
import { lingui } from "@lingui/vite-plugin";
import { LEGACY_BROWSER_TARGET, legacyCss } from "./vite/legacyBrowsers";

// https://vite.dev/config/
const __dirname = dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  base: "/",
  resolve: {
    alias: [
      { find: /^neo-cucumber$/, replacement: resolve(__dirname, "src/public.ts") },
      { find: /^neo-cucumber\/style\.css$/, replacement: resolve(__dirname, "src/App.css") },
      { find: "react", replacement: resolve(__dirname, "node_modules/react") },
      { find: "react-dom", replacement: resolve(__dirname, "node_modules/react-dom") },
      { find: "@lingui/core", replacement: resolve(__dirname, "node_modules/@lingui/core") },
      { find: "@lingui/react", replacement: resolve(__dirname, "node_modules/@lingui/react") },
      { find: "@sentry/browser", replacement: resolve(__dirname, "node_modules/@sentry/browser") },
      { find: "@sentry/react", replacement: resolve(__dirname, "node_modules/@sentry/react") },
      { find: "@iconify/react", replacement: resolve(__dirname, "node_modules/@iconify/react") },
    ],
  },
  server: {
    allowedHosts: true,
  },
  plugins: [
    react({
      plugins: [["@lingui/swc-plugin", {}]],
    }),
    tailwindcss(),
    lingui(),
    legacyCss(),
  ],
  build: {
    // Firefox 56 syntax, for Waterfox Classic; see vite/legacyBrowsers.ts.
    target: LEGACY_BROWSER_TARGET,
    outDir: "dist-example",
    emptyOutDir: true,
    rollupOptions: {
      input: {
        // Standalone example of the package's public API.
        example: resolve(__dirname, "example.html"),
      },
    },
  },
});
