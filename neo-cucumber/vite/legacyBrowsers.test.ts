import { describe, expect, it } from "vitest";
import { parse } from "acorn";
import { build } from "vite";
import type { Rollup } from "vite";
import type { Node } from "acorn";
import offlineConfig from "../vite.offline.config";
import {
  addFlexGapFallback,
  addLegacyFallbacks,
  flattenCascadeLayers,
} from "./legacyBrowsers";

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

describe("addLegacyFallbacks: selector lists", () => {
  /*
   * The bug this exists for. One unknown selector invalidates the whole
   * list, and Tailwind hands its theme to `:root,:host` -- so Firefox 56
   * lost `--color-white` and `--spacing` together, which showed up as a
   * canvas with the page's background instead of white.
   */
  it("keeps the readable half of a list, ahead of the original", () => {
    expect(addLegacyFallbacks(":root,:host{--color-white:#fff}")).toBe(
      ":root{--color-white:#fff}:root,:host{--color-white:#fff}",
    );
  });

  it("keeps a list that mixes plain selectors with :where()", () => {
    expect(
      addLegacyFallbacks("button,input:where([type=button]){appearance:button}"),
    ).toBe(
      "button{appearance:button}button,input:where([type=button]){appearance:button}",
    );
  });

  it("does not split a comma that belongs to :is()", () => {
    const out = addLegacyFallbacks("h1,:is(h2,h3){margin:0}");
    expect(out).toBe("h1{margin:0}h1,:is(h2,h3){margin:0}");
  });

  it("adds nothing when the whole list is unreadable", () => {
    const css = "::file-selector-button{margin:0}";
    expect(addLegacyFallbacks(css)).toBe(css);
  });

  it("adds nothing when every selector is already readable", () => {
    const css = ".a,.b{color:red}";
    expect(addLegacyFallbacks(css)).toBe(css);
  });

  it("reaches rules nested in a conditional group", () => {
    expect(
      addLegacyFallbacks("@media print{:root,:host{--a:1px}}"),
    ).toBe("@media print{:root{--a:1px}:root,:host{--a:1px}}");
  });

  it("does not treat a keyframe step as a selector", () => {
    const css = "@keyframes spin{from{inset:0}to{inset:1px}}";
    expect(addLegacyFallbacks(css)).toBe(css);
  });
});

describe("addLegacyFallbacks: properties after Firefox 56", () => {
  it("expands the logical axis shorthands to their longhands", () => {
    expect(addLegacyFallbacks(".p{padding-inline:4px}")).toBe(
      ".p{padding-inline-start:4px;padding-inline-end:4px;padding-inline:4px}",
    );
    expect(addLegacyFallbacks(".p{padding-block:1px 2px}")).toBe(
      ".p{padding-block-start:1px;padding-block-end:2px;padding-block:1px 2px}",
    );
  });

  it("expands inset by the one-to-four value box rule", () => {
    expect(addLegacyFallbacks(".a{inset:0}")).toBe(
      ".a{top:0;right:0;bottom:0;left:0;inset:0}",
    );
    expect(addLegacyFallbacks(".a{inset:1px 2px 3px}")).toBe(
      ".a{top:1px;right:2px;bottom:3px;left:2px;inset:1px 2px 3px}",
    );
  });

  it("does not split a value on the spaces inside calc()", () => {
    expect(addLegacyFallbacks(".a{inset:calc(var(--s) * 2)}")).toContain(
      "top:calc(var(--s) * 2);right:calc(var(--s) * 2)",
    );
  });

  it("prefixes user-select and tab-size the way NEO does", () => {
    expect(addLegacyFallbacks(".a{user-select:none}")).toBe(
      ".a{-moz-user-select:none;user-select:none}",
    );
    expect(addLegacyFallbacks(".a{tab-size:4}")).toBe(
      ".a{-moz-tab-size:4;tab-size:4}",
    );
  });

  /*
   * `padding:1px;padding-inline:4px` and `padding-inline:4px;padding:1px`
   * compute differently, so the longhands go immediately before the
   * shorthand they stand in for rather than at the top of the block.
   */
  it("keeps each fallback next to the declaration it replaces", () => {
    expect(addLegacyFallbacks(".a{padding:1px;padding-inline:4px}")).toBe(
      ".a{padding:1px;padding-inline-start:4px;padding-inline-end:4px;padding-inline:4px}",
    );
  });

  /*
   * For the two canvases, which are scaled up and smoothed without it. Not
   * for the ground: this went in while chasing NEO's missing grid and did
   * not fix it.
   */
  it("gives image-rendering the spelling an old Gecko knows", () => {
    expect(addLegacyFallbacks(".a{image-rendering:pixelated}")).toBe(
      ".a{image-rendering:-moz-crisp-edges;image-rendering:pixelated}",
    );
    expect(addLegacyFallbacks(".a{image-rendering:crisp-edges}")).toBe(
      ".a{image-rendering:-moz-crisp-edges;image-rendering:crisp-edges}",
    );
  });

  it("leaves image-rendering alone when it is not asking for hard pixels", () => {
    const css = ".a{image-rendering:auto}";
    expect(addLegacyFallbacks(css)).toBe(css);
  });

  /*
   * What actually hid NEO's grid, and only in the light palette: `#bbf` and
   * `14px` run together into a five-character hex run that is not a colour,
   * where `#22223f` and `14px` give a run of eight that is.
   */
  it("restores the space a minifier drops after a closing paren", () => {
    expect(
      addLegacyFallbacks(".g{background-image:linear-gradient(red,var(--a)14px)}"),
    ).toBe(".g{background-image:linear-gradient(red,var(--a) 14px)}");
  });

  it("does not add a space where calc() would change meaning", () => {
    for (const value of ["calc(var(--a)*2)", "calc(var(--a)-2px)", "calc(var(--a)/2)"]) {
      const css = `.a{width:${value}}`;
      expect(addLegacyFallbacks(css)).toBe(css);
    }
  });

  it("does not reach inside a quoted value", () => {
    const css = '.a{content:")x"}';
    expect(addLegacyFallbacks(css)).toBe(css);
  });

  it("leaves a declaration block it has nothing to say about alone", () => {
    const css = ".a{color:red;margin:0}";
    expect(addLegacyFallbacks(css)).toBe(css);
  });
});

describe("addFlexGapFallback", () => {
  const generated = (css: string) => addFlexGapFallback(css).slice(css.length);

  it("spaces a flex row along the inline axis", () => {
    expect(generated(".gap-\\[3px\\]{gap:3px}")).toContain(
      ".gap-\\[3px\\]:not(.flex-col):not(.grid)>*+*{margin-left:3px}",
    );
  });

  it("spaces a flex column along the block axis instead", () => {
    expect(generated(".gap-\\[3px\\]{gap:3px}")).toContain(
      ".flex-col.gap-\\[3px\\]>*+*{margin-top:3px}",
    );
  });

  /*
   * Firefox 56 shipped Grid with `grid-gap`, three versions before the
   * rename to `gap`. Sibling margins would put a gutter before every item
   * but the first of the whole grid, not the first of each row.
   */
  it("gives a grid grid-gap rather than margins", () => {
    const out = generated(".gap-\\[2px\\]{gap:2px}");
    expect(out).toContain(".gap-\\[2px\\]{grid-gap:2px}");
    expect(out).toContain(":not(.grid)");
  });

  it("keeps the two axes apart when they differ", () => {
    const out = generated(".g{row-gap:4px;column-gap:8px}");
    expect(out).toContain(".g{grid-row-gap:4px;grid-column-gap:8px}");
    expect(out).toContain(".g:not(.flex-col):not(.grid)>*+*{margin-left:8px}");
    expect(out).toContain(".flex-col.g>*+*{margin-top:4px}");
  });

  it("reads the two-value gap shorthand as row then column", () => {
    const out = generated(".g{gap:4px 8px}");
    expect(out).toContain("margin-left:8px");
    expect(out).toContain("margin-top:4px");
  });

  it("does not split a value on the spaces inside calc()", () => {
    const out = generated(".g{gap:calc(var(--spacing) * 2)}");
    expect(out).toContain("margin-left:calc(var(--spacing) * 2)");
    expect(out).not.toContain("margin-left:calc(var(--spacing)}");
  });

  it("re-wraps a variant's fallback in the query it came from", () => {
    const out = generated(
      "@media (pointer:coarse){.pointer-coarse\\:gap-\\[5px\\]{gap:5px}}",
    );
    expect(out).toContain("@media (pointer:coarse){.pointer-coarse");
    expect(out).toContain("margin-left:5px");
  });

  it("puts everything behind a query no engine with gap answers", () => {
    const out = generated(".g{gap:1px}");
    expect(out.startsWith("@supports not (row-gap:1px){")).toBe(true);
    expect(out.endsWith("}")).toBe(true);
  });

  it("adds nothing to a sheet that never asks for a gap", () => {
    const css = ".x{color:red}@media print{.y{display:none}}";
    expect(addFlexGapFallback(css)).toBe(css);
  });

  /*
   * `.flex-col` and `.grid` are composed onto the gap class, which only
   * works while the gap class is the entire selector. A hand-written
   * `.panel .row { gap: 2px }` would silently produce `.flex-col.panel .row`,
   * which means something else entirely.
   */
  it("leaves a selector it cannot compose onto alone", () => {
    expect(generated(".panel .row{gap:2px}")).toBe("");
    expect(generated(".a,.b{gap:2px}")).toBe("");
    expect(generated(".a>.b{gap:2px}")).toBe("");
  });

  it("ignores gap-like descriptors that are not style rules", () => {
    expect(generated("@keyframes x{from{gap:2px}to{gap:4px}}")).toBe("");
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

  it("keeps the error reporter in an entry of its own", async () => {
    // frontend/painter/sentry.ts: the SDK assumes an engine newer than the
    // floor, so it is loaded by a script of its own and must not be pulled
    // into offline.js, or into a chunk offline.js imports.
    const chunks = (await built).filter(
      (entry): entry is Rollup.OutputChunk => entry.type === "chunk",
    );
    const reporter = chunks.find((chunk) => chunk.name === "sentry");
    expect(reporter?.isEntry).toBe(true);
    expect(reporter?.code).toContain("ingest.us.sentry.io");

    const painter = chunks.find((chunk) => chunk.name === "offline")!;
    const reachable = [painter, ...painter.imports.map(
      (file) => chunks.find((chunk) => chunk.fileName === file)!,
    )];
    for (const chunk of reachable) {
      expect(chunk.code, chunk.fileName).not.toContain("sentry.io");
    }
  }, 60_000);

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

  /** The one stylesheet the offline bundle emits, as text. */
  async function stylesheet(): Promise<string> {
    const css = (await built).find(
      (entry) => entry.type === "asset" && entry.fileName.endsWith(".css"),
    );
    expect(css?.type).toBe("asset");
    const source = (css as { source: string | Uint8Array }).source;
    return typeof source === "string"
      ? source
      : new TextDecoder().decode(source);
  }

  it("ships no cascade layers, which would take the chrome with them", async () => {
    const text = await stylesheet();

    expect(text).not.toContain("@layer");
    // The sheet is still the whole sheet: the layers were unwrapped, not cut.
    expect(text).toContain("--neo-tool-button");
    expect(text.length).toBeGreaterThan(20_000);
  }, 60_000);

  it("leaves the theme readable without Shadow DOM selectors", async () => {
    const text = await stylesheet();

    /*
     * Tailwind declares its theme on `:root,:host`, and `:host` is Firefox
     * 63 -- one unknown selector voids the whole list, so Firefox 56 lost
     * `--color-white` and `--spacing` at a stroke. The canvas came out the
     * colour of the page behind it because `bg-white` had nothing to resolve.
     */
    expect(text).toMatch(/(^|[;}]):root\{[^}]*--color-white/);
    expect(text).toMatch(/(^|[;}]):root\{[^}]*--spacing/);
  }, 60_000);

  it("draws NEO's grid with stops an old engine can parse", async () => {
    const text = await stylesheet();

    // Two positions on one stop is Firefox 83, and an unparseable gradient
    // takes the whole `background-image` with it -- the ground goes flat.
    const ground = text.match(/\.neo-ground\{[^}]*\}/)?.[0] ?? "";
    expect(ground).toContain("linear-gradient");
    expect(ground).not.toMatch(/transparent\s+0\s+14px/);
  }, 60_000);

  it("spells the post-56 shorthands out as longhands too", async () => {
    const text = await stylesheet();

    // `inset`, the logical axis shorthands and unprefixed `user-select` are
    // all Firefox 66 or later; `mx-auto` on the canvas rides on one of them.
    expect(text).toContain("margin-inline-start:auto");
    expect(text).toMatch(/top:[^;]+;right:[^;]+;bottom:[^;]+;left:[^;]+;inset:/);
    expect(text).toContain("-moz-user-select:");
  }, 60_000);

  it("never lets a value end a function hard against the next token", async () => {
    const text = await stylesheet();

    /*
     * `var(--neo-bk2)14px` is two tokens by the spec and one run of text to
     * an engine that substitutes by re-parsing the string. The light palette
     * turned it into `#bbf14px` and lost NEO's grid; the dark palette's
     * `#22223f14px` happened to survive. Nothing in the sheet may ship that
     * way, whichever colour is standing in the gap.
     */
    expect(text.match(/\)[0-9A-Za-z#]/g) ?? []).toEqual([]);
  }, 60_000);

  it("asks for hard pixels in a spelling Firefox 56 knows", async () => {
    const text = await stylesheet();

    // Every `pixelated` in the sheet, and there is one on the drawing canvas
    // as well as on the ground, arrives with the prefixed form ahead of it.
    const modern = text.match(/image-rendering:pixelated/g) ?? [];
    const legacy = text.match(/image-rendering:-moz-crisp-edges/g) ?? [];
    expect(modern.length).toBeGreaterThan(0);
    expect(legacy.length).toBe(modern.length);
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
