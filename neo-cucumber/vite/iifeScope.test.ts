import { describe, expect, it } from "vitest";
import { runInNewContext } from "node:vm";
import { build } from "vite";
import type { Rollup } from "vite";
import viewerConfig from "../vite.viewer.config";
import siteConfig from "../vite.site.config";

/*
 * The viewer is a classic script on pages with scripts of their own, so
 * every top-level name it declares is one a page can overwrite. It used to
 * declare esbuild's helpers there, and a page's `var b` broke every replay.
 */
describe("the replay viewer bundle", () => {
  it("leaves NeoCucumberReplay as its only global", async () => {
    const result = await build({
      ...viewerConfig,
      // Otherwise vite.config.ts is found and merged in; see
      // legacyBrowsers.test.ts.
      configFile: false,
      logLevel: "silent",
      build: { ...viewerConfig.build, write: false },
    });
    const bundles = (Array.isArray(result) ? result : [result]) as Rollup.RollupOutput[];
    const chunk = bundles
      .flatMap((bundle) => bundle.output)
      .find((entry): entry is Rollup.OutputChunk => entry.type === "chunk");
    if (!chunk) throw new Error("no chunk");

    const globals: Record<string, unknown> = {};
    runInNewContext(chunk.code, globals);
    expect(Object.keys(globals)).toEqual(["NeoCucumberReplay"]);
    expect(typeof (globals.NeoCucumberReplay as { mount?: unknown }).mount).toBe("function");
  }, 60_000);
});

/**
 * A stand-in for whatever a script's top level asks the page for: any
 * property is another stand-in, and so is the result of calling one. Lists
 * are empty and the document is still loading, so nothing is found to work
 * on and every script waits for DOMContentLoaded, which never comes.
 */
function anything(): unknown {
  const stub: unknown = new Proxy(function () {}, {
    get(_target, key) {
      if (key === "readyState") return "loading";
      if (key === "length") return 0;
      if (key === Symbol.iterator) return [][Symbol.iterator];
      if (key === Symbol.toPrimitive) return () => "";
      return stub;
    },
    apply: () => stub,
    construct: () => stub as object,
  });
  return stub;
}

/*
 * base.jinja and the post page load these as classic scripts beside the
 * page's own inline ones, so none may leave a name on `window` -- but htmx,
 * which the page's scripts and the apps call by that name.
 */
describe("the site's own scripts", () => {
  it("declare nothing at the top level but htmx", async () => {
    const result = await build({
      ...siteConfig,
      configFile: false,
      logLevel: "silent",
      build: { ...siteConfig.build, write: false },
    });
    const bundles = (Array.isArray(result) ? result : [result]) as Rollup.RollupOutput[];
    const chunks = bundles
      .flatMap((bundle) => bundle.output)
      .filter((entry): entry is Rollup.OutputChunk => entry.type === "chunk");
    expect(chunks.map((chunk) => chunk.fileName).sort()).toEqual([
      "htmx.js",
      "mention.js",
      "post_zoom.js",
      "reaction_picker.js",
    ]);

    const page = ["document", "navigator", "location", "history", "localStorage", "sessionStorage"];
    for (const chunk of chunks) {
      const globals: Record<string, unknown> = Object.fromEntries(page.map((name) => [name, anything()]));
      // `window` is the global object itself, as it is in a browser, so a
      // name a script puts on it is found among the globals.
      globals.window = globals;
      // What htmx's top level reaches for, and the others' listeners.
      const reached = ["CSSStyleSheet", "XPathEvaluator", "addEventListener", "matchMedia"];
      for (const name of [...reached, "setTimeout", "requestAnimationFrame"]) {
        globals[name] = anything();
      }
      const before = Object.keys(globals);
      runInNewContext(chunk.code, globals);
      const added = Object.keys(globals).filter((name) => !before.includes(name));
      expect(added, chunk.fileName).toEqual(chunk.fileName === "htmx.js" ? ["htmx"] : []);
    }
  }, 60_000);
});
