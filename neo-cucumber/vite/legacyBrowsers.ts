import type { Plugin } from "vite";

/**
 * How old an engine this package's builds still run on, and the CSS pass that
 * gets them there.
 *
 * The engine that sets the floor is Waterfox Classic's, which is Firefox 56's
 * -- the last Gecko with NPAPI, and therefore the browser oekaki users keep
 * around to run the original PaintBBS and ShiPainter applets. Somebody who
 * opens this painter in it is very likely the same person, on the same
 * machine, for the same reason, so it is worth the few kilobytes.
 *
 * This is one target for every build rather than a separate legacy bundle on
 * a user-agent sniff. A second artifact would double the surface that replay
 * fidelity has to hold across, and a `.pch` written by a bundle nobody tests
 * is exactly the failure this codebase can least afford.
 */
export const LEGACY_BROWSER_TARGET = "firefox56";

/**
 * Strip cascade layers, keeping each rule where it already stood.
 *
 * Tailwind v4 emits almost everything it generates inside `@layer theme`,
 * `base`, `components` and `utilities` -- 86% of this package's stylesheet by
 * weight. `@layer` is Firefox 97, and an engine that does not know an at-rule
 * discards the whole block, so on Firefox 56 the painter arrives with a
 * palette and no chrome at all: the toolbox renders as a column of unstyled
 * buttons. Nothing else in the sheet comes close to costing that much.
 *
 * Unwrapping in place is safe because Tailwind already emits the layers in
 * their cascade order, so document order reproduces the layer order. What it
 * does not reproduce is the part of `@layer` that outranks specificity -- a
 * utility no longer beats a more specific base rule just for being a utility.
 * That is precisely the cascade Tailwind v3 had, which is what these same
 * class names were written against.
 *
 * It also repairs `@property`, which Firefox 56 likewise ignores: Tailwind
 * ships a plain-CSS fallback that seeds every `--tw-*` custom property on
 * `*`, guarded by an `@supports` test that is true on exactly the engines
 * lacking `@property` -- but it parks it in `@layer properties`, where the
 * engines that need it cannot see it. Unwrapping hands it back.
 */
export function flattenCascadeLayers(css: string): string {
  let out = "";
  let i = 0;

  while (i < css.length) {
    const ch = css[i];

    // Strings and comments cross untouched, so that a `@layer` written inside
    // one -- `content: "@layer {"` is legal CSS -- is not read as an at-rule.
    if (ch === '"' || ch === "'") {
      const end = skipString(css, i);
      out += css.slice(i, end);
      i = end;
      continue;
    }
    if (ch === "/" && css[i + 1] === "*") {
      const found = css.indexOf("*/", i + 2);
      const end = found === -1 ? css.length : found + 2;
      out += css.slice(i, end);
      i = end;
      continue;
    }
    if (ch !== "@" || !/^@layer\b/i.test(css.slice(i, i + 7))) {
      out += ch;
      i += 1;
      continue;
    }

    // `@layer a, b;` only declares an order. With the layers gone it says
    // nothing, and leaving it behind would be an at-rule Firefox 56 skips.
    const prelude = findPreludeEnd(css, i + 6);
    if (prelude.terminator === ";") {
      i = prelude.at + 1;
      continue;
    }
    if (prelude.terminator === null) {
      out += css.slice(i);
      break;
    }

    // `@layer name { … }`: keep the body, drop the wrapper, and flatten what
    // is inside it -- Tailwind nests `@layer` under `@media` and vice versa.
    const close = findBlockEnd(css, prelude.at);
    out += flattenCascadeLayers(css.slice(prelude.at + 1, close));
    i = close + 1;
  }

  return out;
}

/** Index just past the string literal opening at `start`. */
function skipString(css: string, start: number): number {
  const quote = css[start];
  let i = start + 1;
  while (i < css.length) {
    if (css[i] === "\\") {
      i += 2;
      continue;
    }
    if (css[i] === quote) return i + 1;
    i += 1;
  }
  return i;
}

/** Where an at-rule's prelude stops, and whether it stopped at `{` or `;`. */
function findPreludeEnd(
  css: string,
  from: number,
): { at: number; terminator: "{" | ";" | null } {
  let i = from;
  while (i < css.length) {
    const ch = css[i];
    if (ch === '"' || ch === "'") {
      i = skipString(css, i);
      continue;
    }
    if (ch === "{" || ch === ";") return { at: i, terminator: ch };
    i += 1;
  }
  return { at: i, terminator: null };
}

/** The index of the `}` closing the block that opens at `open`. */
function findBlockEnd(css: string, open: number): number {
  let depth = 0;
  let i = open;
  while (i < css.length) {
    const ch = css[i];
    if (ch === '"' || ch === "'") {
      i = skipString(css, i);
      continue;
    }
    if (ch === "/" && css[i + 1] === "*") {
      const end = css.indexOf("*/", i + 2);
      i = end === -1 ? css.length : end + 2;
      continue;
    }
    if (ch === "{") depth += 1;
    else if (ch === "}") {
      depth -= 1;
      if (depth === 0) return i;
    }
    i += 1;
  }
  return css.length;
}

/**
 * Runs {@link flattenCascadeLayers} over every stylesheet a build emits.
 *
 * `enforce: "post"` and `generateBundle` put it after Vite has minified the
 * CSS to `build.cssTarget`, so this rewrites the bytes that ship rather than
 * something esbuild still gets a turn at.
 */
export function legacyCss(): Plugin {
  return {
    name: "neo-cucumber:legacy-css",
    enforce: "post",
    generateBundle(_options, bundle) {
      for (const asset of Object.values(bundle)) {
        if (asset.type !== "asset" || !asset.fileName.endsWith(".css")) continue;
        const source =
          typeof asset.source === "string"
            ? asset.source
            : new TextDecoder().decode(asset.source);
        asset.source = flattenCascadeLayers(source);
      }
    },
  };
}
