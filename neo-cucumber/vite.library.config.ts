import { defineConfig } from "vite";
import react from "@vitejs/plugin-react-swc";
import tailwindcss from "@tailwindcss/vite";
import { lingui } from "@lingui/vite-plugin";
import { LEGACY_BROWSER_TARGET, legacyCss } from "./vite/legacyBrowsers";
import { resolve } from "node:path";

export default defineConfig({
  plugins: [
    react({ plugins: [["@lingui/swc-plugin", {}]] }),
    tailwindcss(),
    lingui(),
    legacyCss(),
  ],
  build: {
    // Firefox 56 syntax, for Waterfox Classic; see vite/legacyBrowsers.ts.
    target: LEGACY_BROWSER_TARGET,
    outDir: "dist-lib",
    emptyOutDir: true,
    lib: {
      entry: resolve(import.meta.dirname, "src/public.ts"),
      formats: ["es"],
      fileName: () => "neo-cucumber.js",
      cssFileName: "style",
    },
    rollupOptions: {
      external: (id) =>
        id === "react" ||
        id.startsWith("react/") ||
        id === "react-dom" ||
        id.startsWith("react-dom/"),
    },
  },
});
