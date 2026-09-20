import { describe, expect, it } from "vitest";
import { parse } from "acorn";
import { build } from "vite";
import type { Rollup } from "vite";
import type { Node } from "acorn";
import offlineConfig from "../vite.offline.config";
import { flattenCascadeLayers } from "./legacyBrowsers";

describe("flattenCascadeLayers", () => {
  it("unwraps a layer and keeps what was in it", () => {
    expect(flattenCascadeLayers("@layer utilities{.a{color:red}}")).toBe(
      ".a{color:red}",
    );
  });

  it("drops a bare layer-order statement", () => {
    expect(flattenCascadeLayers("@layer a,b;.a{color:red}")).toBe(
      ".a{color:red}",
    );
  });

  it("keeps rules in the order the layers put them in", () => {
    expect(
      flattenCascadeLayers(
        "@layer base{.x{color:red}}@layer utilities{.x{color:blue}}",
      ),
    ).toBe(".x{color:red}.x{color:blue}");
  });

  it("unwraps layers nested in each other and under a media query", () => {
    expect(
      flattenCascadeLayers(
        "@layer a{@media (min-width:40rem){@layer b{.x{color:red}}}}",
      ),
    ).toBe("@media (min-width:40rem){.x{color:red}}");
  });

  it("leaves other at-rules and their bodies alone", () => {
    const css = "@supports (display:grid){.x{display:grid}}@media print{.y{}}";
    expect(flattenCascadeLayers(css)).toBe(css);
  });

  /*
   * `content` can hold anything, including something that reads like the
   * at-rule being removed. Scanning for `@layer` without tracking strings
   * would treat the opening quote's braces as a block and swallow the rules
   * after it.
   */
  it("does not read a @layer written inside a string or a comment", () => {
    expect(flattenCascadeLayers('.x{content:"@layer q{"}.y{color:red}')).toBe(
      '.x{content:"@layer q{"}.y{color:red}',
    );
    expect(flattenCascadeLayers("/* @layer q{ */.y{color:red}")).toBe(
      "/* @layer q{ */.y{color:red}",
    );
  });

  it("preserves every declaration it passes over", () => {
    const css =
      '@layer theme{:root{--a:1px}}@layer base{*{box-sizing:border-box}}' +
      '@layer utilities{.p{padding:var(--a)}.c{content:"}"}}';
    const flattened = flattenCascadeLayers(css);
    for (const declaration of ["--a:1px", "box-sizing:border-box", "padding:var(--a)"]) {
      expect(flattened).toContain(declaration);
    }
    expect(flattened).not.toContain("@layer");
  });
});

/*
 * What actually ships, checked as it ships.
 *
 * Every constraint below is invisible to `tsc` and to the unit tests: the
 * syntax comes out of esbuild's downlevelling, and the CSS out of Tailwind
 * and the plugin above. A dependency bump is the likely way any of it comes
 * back, and the symptom in Waterfox Classic is a blank page with one
 * SyntaxError -- which is why this builds the real bundle rather than
 * asserting about the configuration that produces it.
 */
describe("the offline bundle, as Firefox 56 would read it", () => {
  // One build, both assertions. `build()` answers with a bundle per output
  // format, so the results are pooled rather than indexed into.
  const built = (async () => {
    const result = await build({
      ...offlineConfig,
      // Without this, `build()` finds vite.config.ts and merges it in, and
      // the assertions below end up describing that file's settings rather
      // than the offline bundle's -- passing even when this config has lost
      // every one of them.
      configFile: false,
      logLevel: "silent",
      build: { ...offlineConfig.build, write: false },
    });
    const bundles = (
      Array.isArray(result) ? result : [result]
    ) as Rollup.RollupOutput[];
    return bundles.reduce<(Rollup.OutputChunk | Rollup.OutputAsset)[]>(
      (all, bundle) => all.concat(bundle.output),
      [],
    );
  })();

  it("parses as ES2018, the last grammar Firefox 56 accepts", async () => {
    const chunk = (await built).find(
      (entry) => entry.type === "chunk" && entry.isEntry,
    );
    expect(chunk?.type).toBe("chunk");
    const code = (chunk as { code: string }).code;

    /*
     * Firefox 56 is ES2017 plus object rest/spread, which landed in 55. The
     * next things along are exactly what this package kept tripping over:
     * optional catch binding (58), nullish coalescing and optional chaining
     * (72 and 74), and class fields (69) -- ES2019 and later, so a parser
     * pinned to 2018 rejects all of them.
     */
    expect(() =>
      parse(code, { ecmaVersion: 2018, sourceType: "module" }),
    ).not.toThrow();

    // The parts of ES2018 that Firefox 56 does not have, which a 2018 parser
    // accepts and so cannot speak for: async iteration arrived in 57, and the
    // regular-expression additions in 78.
    const offenders: string[] = [];
    walk(parse(code, { ecmaVersion: 2018, sourceType: "module" }), (node) => {
      const n = node as Node & {
        await?: boolean;
        async?: boolean;
        generator?: boolean;
        regex?: { pattern: string; flags: string };
      };
      if (n.type === "ForOfStatement" && n.await) offenders.push("for await");
      if (n.async && n.generator) offenders.push("async generator");
      if (n.regex) {
        if (n.regex.flags.includes("s")) offenders.push("regex dotAll flag");
        if (/\(\?</.test(n.regex.pattern)) {
          offenders.push("regex named group or lookbehind");
        }
        if (/\\[pP]\{/.test(n.regex.pattern)) {
          offenders.push("regex unicode property escape");
        }
      }
    });
    expect(offenders).toEqual([]);
  }, 60_000);

  it("ships no cascade layers, which would take the chrome with them", async () => {
    const css = (await built).find(
      (entry) => entry.type === "asset" && entry.fileName.endsWith(".css"),
    );
    expect(css?.type).toBe("asset");
    const source = (css as { source: string | Uint8Array }).source;
    const text =
      typeof source === "string" ? source : new TextDecoder().decode(source);

    expect(text).not.toContain("@layer");
    // The sheet is still the whole sheet: the layers were unwrapped, not cut.
    expect(text).toContain("--neo-tool-button");
    expect(text.length).toBeGreaterThan(20_000);
  }, 60_000);
});

/** Every node under `root`, without pulling in a walker dependency. */
function walk(root: Node, visit: (node: Node) => void): void {
  visit(root);
  for (const value of Object.values(root as unknown as Record<string, unknown>)) {
    if (Array.isArray(value)) {
      for (const member of value) {
        if (isNode(member)) walk(member, visit);
      }
    } else if (isNode(value)) {
      walk(value, visit);
    }
  }
}

function isNode(value: unknown): value is Node {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { type?: unknown }).type === "string"
  );
}
