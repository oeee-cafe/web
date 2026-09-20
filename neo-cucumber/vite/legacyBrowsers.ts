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
 * Selector syntax Firefox 56 cannot parse.
 *
 * `:host` is Shadow DOM, 63. `:is()` and `:where()` are 78, `:has()` 121,
 * `::file-selector-button` 82. `::backdrop` is in here because it rides along
 * in Tailwind's reset lists and costs nothing to be careful about.
 */
const UNPARSEABLE_SELECTOR =
  /:host\b|:is\(|:where\(|:has\(|::file-selector-button|::backdrop/i;

/**
 * Keep the half of a selector list that Firefox 56 can read.
 *
 * One unknown selector invalidates the *entire* list -- that is the CSS
 * spec's own error handling, not a quirk -- and Tailwind v4 hands its theme
 * to `:root,:host`. On Firefox 56 that rule vanishes whole, taking
 * `--color-white`, `--color-black` and `--spacing` with it. The visible
 * result is a canvas with no white to paint on, because `bg-white` resolves
 * to `var(--color-white)` and there is no longer any such thing, and every
 * utility built on the spacing scale silently computing to nothing.
 *
 * So the readable selectors are emitted a second time, as their own rule,
 * directly before the original. A browser that understands the whole list
 * applies both and the later one settles every tie with identical values;
 * Firefox 56 sees only the copy. Nothing is removed, because the original
 * is still what modern engines should be reading.
 */
function legacySelectorList(selector: string): string | null {
  const parts = splitTopLevel(selector, ",");
  if (!parts.some((part) => UNPARSEABLE_SELECTOR.test(part))) return null;
  const readable = parts.filter((part) => !UNPARSEABLE_SELECTOR.test(part));
  return readable.length ? readable.join(",") : null;
}

/**
 * Longhands and prefixes for shorthands that arrived after Firefox 56.
 *
 * Each entry turns one declaration into the declarations that say the same
 * thing to an older engine. They are emitted immediately before the original,
 * never hoisted to the top of the rule: `padding:1px;padding-inline:4px` means
 * something different from `padding-inline:4px;padding:1px`, and a fallback
 * that reorders declarations is a fallback that changes the answer.
 */
const LEGACY_DECLARATIONS: Record<
  string,
  (value: string) => Record<string, string> | null
> = {
  // The logical shorthands are Firefox 66; the longhands under them are 41.
  "padding-inline": (v) => axis(v, "padding-inline-start", "padding-inline-end"),
  "padding-block": (v) => axis(v, "padding-block-start", "padding-block-end"),
  "margin-inline": (v) => axis(v, "margin-inline-start", "margin-inline-end"),
  "margin-block": (v) => axis(v, "margin-block-start", "margin-block-end"),
  // `inset` is Firefox 66. `mx-auto` on the canvas rides on this too.
  inset: (v) => box(v),
  // Unprefixed `user-select` is Firefox 69 -- NEO writes all four prefixes on
  // `.NEO` for the same reason. `tab-size` unprefixed is 91.
  "user-select": (v) => ({ "-moz-user-select": v }),
  "tab-size": (v) => ({ "-moz-tab-size": v }),
  /*
   * `pixelated` is Firefox 93 and `crisp-edges` 65; `-moz-crisp-edges` has
   * been there since 3.6. Dropping the declaration does not leave a painter
   * slightly worse, it leaves it smoothed: the drawing canvas and the replay
   * canvas are both scaled up and both say `pixelated` to stop exactly that.
   *
   * This was added while chasing NEO's missing grid and is not what fixed
   * it -- that was `separateAfterFunctions()` below. It stays because the
   * two canvases need it on its own account, not because it explains
   * anything about the ground.
   */
  "image-rendering": (v) =>
    v === "pixelated" || v === "crisp-edges"
      ? { "image-rendering": "-moz-crisp-edges" }
      : null,
};

/** `<start> [<end>]`, the way a logical axis shorthand is written. */
function axis(
  value: string,
  start: string,
  end: string,
): Record<string, string> | null {
  const parts = splitTopLevel(value, " ");
  if (parts.length === 0 || parts.length > 2) return null;
  return { [start]: parts[0], [end]: parts[1] ?? parts[0] };
}

/** The one-to-four value box rule, as `inset` uses it. */
function box(value: string): Record<string, string> | null {
  const p = splitTopLevel(value, " ");
  if (p.length === 0 || p.length > 4) return null;
  const top = p[0];
  const right = p[1] ?? top;
  const bottom = p[2] ?? top;
  const left = p[3] ?? right;
  return { top, right, bottom, left };
}

/**
 * Put back the space a minifier drops after a closing parenthesis.
 *
 * `var(--neo-bk2)14px` is two tokens to a tokenizer that follows the spec,
 * so esbuild is right to save the byte. Gecko's first custom-property
 * implementation substituted by re-serializing the value and parsing the
 * text again, and that turns the pair into one run of characters.
 *
 * Which is why NEO's grid went missing in the light palette and not the dark
 * one, from a single rule that says nothing about either: `#bbf` and `14px`
 * run together into `#bbf14px`, whose hex run is five characters and not a
 * colour, while `#22223f` and `14px` give `#22223f14px`, whose run of eight
 * is. One theme's ground kept its line and the other's lost the whole
 * `background-image`.
 *
 * A space is added only before a character that could have joined the
 * previous token -- never before `-`, `+`, `*` or `/`, because inside
 * `calc()` the whitespace around those is part of the grammar and
 * `calc(var(--a)-2px)` does not mean `calc(var(--a) -2px)`.
 */
function separateAfterFunctions(value: string): string {
  let out = "";
  let i = 0;

  while (i < value.length) {
    const ch = value[i];
    if (ch === '"' || ch === "'") {
      const end = skipString(value, i);
      out += value.slice(i, end);
      i = end;
      continue;
    }
    out += ch;
    if (ch === ")" && /[0-9A-Za-z#]/.test(value[i + 1] ?? "")) out += " ";
    i += 1;
  }

  return out;
}

/** Rewrite one declaration block, adding what Firefox 56 needs as it goes. */
function legacyDeclarations(body: string): string {
  let changed = false;
  const out: string[] = [];

  for (const declaration of splitTopLevel(body, ";")) {
    const colon = declaration.indexOf(":");
    if (colon === -1) {
      out.push(declaration);
      continue;
    }

    const property = declaration.slice(0, colon).trim();
    const value = declaration.slice(colon + 1).trim();
    const spaced = separateAfterFunctions(value);

    const legacy = LEGACY_DECLARATIONS[property]?.(spaced);
    if (legacy) {
      for (const [name, replacement] of Object.entries(legacy)) {
        out.push(`${name}:${replacement}`);
      }
      changed = true;
    }

    if (spaced === value) {
      out.push(declaration);
    } else {
      out.push(`${property}:${spaced}`);
      changed = true;
    }
  }

  return changed ? out.join(";") : body;
}

/**
 * Everything above, over one stylesheet.
 *
 * Both passes only ever add: no rule is dropped and no declaration is
 * rewritten in place, so an engine that understood the input still computes
 * exactly what it computed before.
 */
export function addLegacyFallbacks(css: string): string {
  return rewriteStyleRules(css, (selector, body) => {
    const patched = legacyDeclarations(body);
    const legacy = legacySelectorList(selector);
    const original = `${selector}{${patched}}`;
    return legacy ? `${legacy}{${patched}}${original}` : original;
  });
}

/**
 * Rewrite every style rule, descending through the conditional groups.
 *
 * `@keyframes`, `@font-face` and `@property` are stepped over rather than
 * into: their contents look like rules and declarations but are neither, and
 * a `from{}` is not a selector.
 */
function rewriteStyleRules(
  css: string,
  transform: (selector: string, body: string) => string,
): string {
  let out = "";
  let i = 0;
  let start = 0;

  while (i < css.length) {
    const ch = css[i];
    if (ch === '"' || ch === "'") {
      i = skipString(css, i);
      continue;
    }
    if (ch === "/" && css[i + 1] === "*") {
      const found = css.indexOf("*/", i + 2);
      i = found === -1 ? css.length : found + 2;
      continue;
    }
    if (ch === ";") {
      out += css.slice(start, i + 1);
      i += 1;
      start = i;
      continue;
    }
    if (ch !== "{") {
      i += 1;
      continue;
    }

    const prelude = css.slice(start, i);
    const selector = prelude.trim();
    const indent = prelude.slice(0, prelude.length - prelude.trimStart().length);
    const close = findBlockEnd(css, i);
    const body = css.slice(i + 1, close);

    if (selector.startsWith("@")) {
      out += /^@(media|supports|container|layer|document)\b/i.test(selector)
        ? `${prelude}{${rewriteStyleRules(body, transform)}}`
        : `${prelude}{${body}}`;
    } else if (selector) {
      out += indent + transform(selector, body);
    } else {
      out += `${prelude}{${body}}`;
    }

    i = close + 1;
    start = i;
  }

  return out + css.slice(start);
}

/**
 * The engines this spacing fallback is for, and no others.
 *
 * Flexbox `gap` is Firefox 63. There is no feature query for it, so this
 * tests unprefixed `row-gap`, which arrived in 61 when Grid's `grid-gap` was
 * renamed. The two disagree only for 61 and 62, and what those get is the
 * spacing they would have had anyway.
 */
const NO_FLEX_GAP = "@supports not (row-gap:1px)";

/**
 * Space rows and columns the way NEO does, for engines without `gap`.
 *
 * NEO never asks a container to distribute space; the spacing belongs to the
 * item. `.toolTipOff` carries `margin-top: 3px`, `.colorTipOff` carries
 * `margin-right: 4px`, `.layerControl` carries `margin-top: 6px`. That is
 * what this reproduces -- a margin on each item after the first -- except
 * that it is selected for rather than written onto every element, so a
 * conditionally rendered button cannot leave a gap behind it and the call
 * sites keep using `gap-*` like anything else in this package.
 *
 * Grids are handled by `grid-gap` instead, which Firefox 56 has had since
 * Grid shipped in 52 and which gets multi-row spacing exactly right where a
 * sibling margin would put a gutter before every item but the row's first.
 *
 * Everything emitted here sits inside `@supports not (row-gap:1px)`, so no
 * engine that has `gap` ever reads a rule of it: the modern cascade is
 * untouched, byte for byte, which is not something a change to 49 call sites
 * could have promised.
 *
 * The one case it does not reproduce is a wrapping row, where the items of a
 * second line keep the leading margin `gap` would have dropped.
 */
export function addFlexGapFallback(css: string): string {
  const blocks: string[] = [];

  forEachStyleRule(css, (selector, body, media) => {
    // Every Tailwind gap utility is a single class, which is what makes
    // `.flex-col` and `.grid` below composable with it. Anything else is
    // hand-written and left alone.
    if (!/^\.[^\s,>+~]+$/.test(selector)) return;

    const gaps = readGaps(body);
    if (!gaps.row && !gaps.column) return;

    const rules: string[] = [];
    const legacy: string[] = [];
    if (gaps.row && gaps.column && gaps.row === gaps.column) {
      legacy.push(`grid-gap:${gaps.row}`);
    } else {
      if (gaps.row) legacy.push(`grid-row-gap:${gaps.row}`);
      if (gaps.column) legacy.push(`grid-column-gap:${gaps.column}`);
    }
    rules.push(`${selector}{${legacy.join(";")}}`);

    /*
     * A flex row, the default direction, spaces along the inline axis;
     * `flex-col` spaces along the block one; a grid took `grid-gap` above and
     * wants neither.
     *
     * Which container a rule is for is settled by `:not()` rather than by
     * setting a margin and then zeroing it again further down. A reset would
     * outrank the `ml-*` and `mt-*` utilities a child may carry of its own --
     * `NeoWindow`'s title label is a `ml-[4px]` span inside a `gap-[3px]`
     * row -- and silently drop them on exactly the browsers this is for.
     */
    if (gaps.column) {
      rules.push(
        `${selector}:not(.flex-col):not(.grid)>*+*{margin-left:${gaps.column}}`,
      );
    }
    if (gaps.row) {
      rules.push(`.flex-col${selector}>*+*{margin-top:${gaps.row}}`);
    }

    const emitted = rules.join("");
    blocks.push(media.length ? `${media.join("{")}{${emitted}}` : emitted);
  });

  return blocks.length ? `${css}${NO_FLEX_GAP}{${blocks.join("")}}` : css;
}

/** The row and column gaps a declaration block asks for, if any. */
function readGaps(body: string): { row?: string; column?: string } {
  const gaps: { row?: string; column?: string } = {};
  for (const declaration of splitTopLevel(body, ";")) {
    const colon = declaration.indexOf(":");
    if (colon === -1) continue;
    const property = declaration.slice(0, colon).trim();
    const value = declaration.slice(colon + 1).trim();
    if (!value) continue;

    if (property === "row-gap") gaps.row = value;
    else if (property === "column-gap") gaps.column = value;
    else if (property === "gap") {
      // `gap: <row> <column>`, or one value standing for both.
      const parts = splitTopLevel(value, " ").filter(Boolean);
      gaps.row = parts[0];
      gaps.column = parts[1] ?? parts[0];
    }
  }
  return gaps;
}

/** Split on `separator`, ignoring any that sit inside brackets or quotes. */
function splitTopLevel(value: string, separator: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  let i = 0;
  while (i < value.length) {
    const ch = value[i];
    if (ch === '"' || ch === "'") {
      i = skipString(value, i);
      continue;
    }
    if (ch === "(" || ch === "[") depth += 1;
    else if (ch === ")" || ch === "]") depth -= 1;
    else if (ch === separator && depth === 0) {
      parts.push(value.slice(start, i).trim());
      start = i + 1;
    }
    i += 1;
  }
  parts.push(value.slice(start).trim());
  return parts.filter((part) => part.length > 0);
}

/**
 * Visit every style rule, with the at-rules it is nested in.
 *
 * `visit` receives the selector, the declaration block, and the preludes of
 * the enclosing conditional rules, outermost first -- `gap` utilities reach
 * this sheet under `@media` for their responsive and `pointer-coarse`
 * variants, and a fallback for one has to be re-wrapped in the same query.
 */
function forEachStyleRule(
  css: string,
  visit: (selector: string, body: string, media: string[]) => void,
  media: string[] = [],
): void {
  let i = 0;
  let start = 0;

  while (i < css.length) {
    const ch = css[i];
    if (ch === '"' || ch === "'") {
      i = skipString(css, i);
      continue;
    }
    if (ch === "/" && css[i + 1] === "*") {
      const found = css.indexOf("*/", i + 2);
      i = found === -1 ? css.length : found + 2;
      continue;
    }
    if (ch === ";") {
      // A statement rather than a block -- `@charset "utf-8";` and the like.
      // What came before it is not the prelude of anything.
      start = i + 1;
      i += 1;
      continue;
    }
    if (ch !== "{") {
      i += 1;
      continue;
    }

    const prelude = css.slice(start, i).trim();
    const close = findBlockEnd(css, i);
    const body = css.slice(i + 1, close);

    if (prelude.startsWith("@")) {
      // Only conditional groups contain style rules worth descending into.
      // `@keyframes` percentages and `@property` descriptors are not rules.
      if (/^@(media|supports|container|layer)\b/i.test(prelude)) {
        forEachStyleRule(body, visit, [...media, prelude]);
      }
    } else if (prelude) {
      visit(prelude, body, media);
    }

    i = close + 1;
    start = i;
  }
}

/**
 * Runs the Firefox 56 rewrites over every stylesheet a build emits.
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
        // Layers first: the later passes read the rules they are repairing,
        // and have to see them at the depth they will ship at.
        asset.source = addFlexGapFallback(
          addLegacyFallbacks(flattenCascadeLayers(source)),
        );
      }
    },
  };
}
