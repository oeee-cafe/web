import { describe, expect, it } from "vitest";
import { runInNewContext } from "node:vm";
import { build } from "vite";
import type { Rollup } from "vite";
import viewerConfig from "../vite.viewer.config";

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
