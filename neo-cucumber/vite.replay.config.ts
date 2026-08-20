import { defineConfig } from "vite";
import { resolve } from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react-swc";
import { lingui } from "@lingui/vite-plugin";

const packageRoot = import.meta.dirname;
const replayRoot = resolve(packageRoot, "../frontend/replay");

/**
 * The staff-only viewer for a recorded collaborative session.
 *
 * React is here because the painter is a React component even when the page
 * around it is not; the page itself is plain DOM, since it is a handful of
 * buttons over a canvas.
 */
export default defineConfig({
  // The painter selects production branches of React and Lingui at build
  // time; without this the bundle drags in the Lingui config loader, which
  // wants `node:module` and does not belong in a browser.
  define: { "process.env.NODE_ENV": JSON.stringify("production") },
  root: replayRoot,
  base: "/static/replay/",
  resolve: {
    alias: [
      { find: /^neo-cucumber$/, replacement: resolve(packageRoot, "src/public.ts") },
      { find: /^neo-cucumber\/style\.css$/, replacement: resolve(packageRoot, "src/App.css") },
      { find: "react", replacement: resolve(packageRoot, "node_modules/react") },
      { find: "react-dom", replacement: resolve(packageRoot, "node_modules/react-dom") },
      { find: "@lingui/core", replacement: resolve(packageRoot, "node_modules/@lingui/core") },
      { find: "@lingui/react", replacement: resolve(packageRoot, "node_modules/@lingui/react") },
      { find: "@iconify/react", replacement: resolve(packageRoot, "node_modules/@iconify/react") },
    ],
  },
  plugins: [
    react({ plugins: [["@lingui/swc-plugin", {}]] }),
    tailwindcss(),
    lingui(),
  ],
  build: {
    outDir: resolve(packageRoot, "dist-replay"),
    emptyOutDir: true,
    rollupOptions: {
      input: { replay: resolve(replayRoot, "index.html") },
    },
  },
});
