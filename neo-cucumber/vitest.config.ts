import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react-swc";
import tailwindcss from "@tailwindcss/vite";
import { playwright } from "@vitest/browser-playwright";
import path from "node:path";
import { lingui } from "@lingui/vite-plugin";

// The canonical NEO sources live outside this package and are loaded as raw
// text by the fidelity tests, so Vite has to be allowed to serve them.
const repoRoot = path.resolve(import.meta.dirname, "..");

export default defineConfig({
  resolve: {
    alias: [
      {
        find: "neo-cucumber/internal",
        replacement: path.resolve(import.meta.dirname, "src"),
      },
      // The host applications under ../frontend are tested from here, and they
      // import the package the way a published consumer would.
      {
        find: /^neo-cucumber$/,
        replacement: path.resolve(import.meta.dirname, "src/public.ts"),
      },
      {
        find: /^neo-cucumber\/style\.css$/,
        replacement: path.resolve(import.meta.dirname, "src/App.css"),
      },
      { find: "react", replacement: path.resolve(import.meta.dirname, "node_modules/react") },
      {
        find: "react-dom",
        replacement: path.resolve(import.meta.dirname, "node_modules/react-dom"),
      },
    ],
  },
  // Tailwind too: component tests render the real components, and without it
  // they render unstyled -- which quietly turns any assertion about layout or
  // colour into an assertion about the browser's defaults.
  plugins: [
    react({ plugins: [["@lingui/swc-plugin", {}]] }),
    tailwindcss(),
    lingui(),
  ],
  server: {
    fs: {
      allow: [repoRoot],
    },
  },
  test: {
    projects: [
      {
        // Pure logic: replay format, action bookkeeping
        extends: true,
        test: {
          name: "node",
          environment: "node",
          include: [
            "src/**/*.test.ts",
            "../frontend/**/*.test.ts",
            // The build-time helpers that keep the bundles readable by
            // Firefox 56, and a check on the bundles they produce.
            "vite/**/*.test.ts",
          ],
          exclude: [
            "src/**/*.browser.test.{ts,tsx}",
            "../frontend/**/*.browser.test.{ts,tsx}",
            "vite/**/*.browser.test.ts",
          ],
        },
      },
      {
        // Anything needing a real canvas or a real React render
        extends: true,
        // react-dom/client and the JSX runtimes are CommonJS, so they have to be
        // pre-bundled for the browser to import them as ESM.
        optimizeDeps: {
          include: [
            "react",
            "react-dom",
            "react-dom/client",
            "react/jsx-runtime",
            "react/jsx-dev-runtime",
          ],
        },
        test: {
          name: "browser",
          // The hosts under ../frontend are tested from here too, the way the
          // node project already tests them: what a drawing page's stylesheet
          // does to the painter is a claim about the page, not the package.
          include: [
            "src/**/*.browser.test.{ts,tsx}",
            "../frontend/**/*.browser.test.{ts,tsx}",
            // The Firefox 56 gap fallback, measured against real layout.
            "vite/**/*.browser.test.ts",
          ],
          /*
           * One file at a time.
           *
           * The corpus test replays 1800 real .pch files through two engines
           * in one page, and running it alongside the other browser files
           * killed the page outright -- "Browser connection was closed" --
           * in two of four full runs once the suite grew past twenty files.
           * A crash that only appears under load looks exactly like a flaky
           * assertion, and re-rolling it is not a verification strategy.
           */
          fileParallelism: false,
          browser: {
            enabled: true,
            provider: playwright(),
            headless: true,
            instances: [{ browser: "chromium" }],
          },
        },
      },
    ],
  },
});
