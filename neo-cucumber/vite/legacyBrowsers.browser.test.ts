import { afterEach, describe, expect, it } from "vitest";
import { addFlexGapFallback } from "./legacyBrowsers";

/*
 * Does the fallback actually lay out like `gap`?
 *
 * The unit tests say what CSS comes out; they cannot say whether it puts the
 * items anywhere near where `gap` would. This measures both -- once with a
 * real `gap`, once with the fallback and no `gap` at all -- and compares the
 * boxes. The browser here is Chromium, which has `gap`, so the fallback is
 * lifted out of its `@supports` guard to make it apply; what is under test is
 * the spacing the rules describe, which is the same wherever they are read.
 *
 * Firefox 56 is not in this room, so this cannot prove the guard's own
 * behaviour. What it can prove is the part that would otherwise be argued
 * from the shape of the CSS rather than from a measurement.
 */

/** Tailwind's display and direction utilities, as the fallback expects them. */
const BASE =
  ".flex{display:flex}.flex-col{flex-direction:column}.grid{display:grid}";

const ITEM = "display:block;width:10px;height:10px";

const mounted: HTMLElement[] = [];

afterEach(() => {
  while (mounted.length) mounted.pop()?.remove();
});

/**
 * Everything `addFlexGapFallback` appended, with the `@supports` unwrapped.
 *
 * `on: "flex"` also drops the `grid-gap` rules. They are inert on a flex
 * container in Firefox 56, which is why the fallback can emit them
 * unconditionally -- but Chromium kept `grid-gap` as a live alias for `gap`
 * on every display type, so leaving them in would space these rows twice and
 * measure a browser nothing runs in. Production never meets this: the guard
 * keeps the whole block away from anything that has `gap`.
 */
function fallbackFor(gapRule: string, on: "flex" | "grid"): string {
  const generated = addFlexGapFallback(gapRule).slice(gapRule.length);
  const open = generated.indexOf("{");
  const inner = generated.slice(open + 1, generated.lastIndexOf("}"));
  return on === "grid"
    ? inner
    : inner.replace(/\.[^{}]*\{grid-(?:row-|column-)?gap:[^{}]*\}/g, "");
}

/**
 * Each child's offset from the container's top-left, after layout.
 *
 * Torn down before returning rather than in `afterEach`: a `<style>` applies
 * to the whole document, so leaving the reference sheet mounted would hand
 * its real `gap` to the fallback's container as well, and the comparison
 * would be a container against itself plus some margins.
 */
function layout(
  sheet: string,
  containerClass: string,
  children: number,
): number[][] {
  const host = document.createElement("div");
  host.innerHTML =
    `<style>${sheet}</style>` +
    `<div class="${containerClass}" style="position:absolute;top:0;left:0">` +
    Array.from({ length: children }, () => `<span style="${ITEM}"></span>`).join(
      "",
    ) +
    `</div>`;
  document.body.appendChild(host);

  try {
    const container = host.querySelector("div")!;
    const origin = container.getBoundingClientRect();
    return Array.from(container.children).map((child) => {
      const box = child.getBoundingClientRect();
      return [box.left - origin.left, box.top - origin.top];
    });
  } finally {
    host.remove();
  }
}

describe("the gap fallback lays out the way gap does", () => {
  it("spaces a flex row identically", () => {
    const gapRule = ".gap-\\[3px\\]{gap:3px}";
    const withGap = layout(BASE + gapRule, "flex gap-[3px]", 3);
    const withFallback = layout(
      BASE + fallbackFor(gapRule, "flex"),
      "flex gap-[3px]",
      3,
    );

    expect(withGap).toEqual([
      [0, 0],
      [13, 0],
      [26, 0],
    ]);
    expect(withFallback).toEqual(withGap);
  });

  it("spaces a flex column identically", () => {
    const gapRule = ".gap-\\[6px\\]{gap:6px}";
    const withGap = layout(BASE + gapRule, "flex flex-col gap-[6px]", 3);
    const withFallback = layout(
      BASE + fallbackFor(gapRule, "flex"),
      "flex flex-col gap-[6px]",
      3,
    );

    expect(withGap).toEqual([
      [0, 0],
      [0, 16],
      [0, 32],
    ]);
    expect(withFallback).toEqual(withGap);
  });

  /*
   * The case sibling margins get wrong and `grid-gap` gets right: the third
   * item opens a new row, and a margin would push it in from the left.
   */
  it("spaces a two-column grid identically, without indenting a new row", () => {
    const gapRule = ".gap-\\[2px\\]{gap:2px}";
    const columns = "grid-template-columns:10px 10px";
    const withGap = layout(
      BASE + gapRule + `.cols{${columns}}`,
      "grid cols gap-[2px]",
      4,
    );
    const withFallback = layout(
      BASE + fallbackFor(gapRule, "grid") + `.cols{${columns}}`,
      "grid cols gap-[2px]",
      4,
    );

    expect(withGap).toEqual([
      [0, 0],
      [12, 0],
      [0, 12],
      [12, 12],
    ]);
    expect(withFallback).toEqual(withGap);
  });

  /*
   * Where the fallback and a child's own margin meet.
   *
   * `gap` adds to a child's margin; a margin cannot add to itself, so on the
   * spaced axis the fallback's rule is more specific and takes it over --
   * `NeoWindow`'s `ml-[4px]` title sits 3px from the dots on Firefox 56
   * instead of 7px. The axis it is not spacing is never touched, which is
   * what keeps the `mb-*` used throughout the modals intact.
   *
   * Pinned here because it is a real difference rather than an oversight,
   * and because narrowing it later should be a decision somebody makes on
   * purpose.
   */
  it("takes over a same-axis margin, and leaves the other axis alone", () => {
    const gapRule = ".gap-\\[3px\\]{gap:3px}";
    const sheet =
      BASE +
      fallbackFor(gapRule, "flex") +
      ".mt-\\[9px\\]{margin-top:9px}.mb-\\[9px\\]{margin-bottom:9px}";

    const host = document.createElement("div");
    host.innerHTML =
      `<style>${sheet}</style>` +
      `<div class="flex flex-col gap-[3px]">` +
      `<span style="${ITEM}"></span>` +
      `<span class="mt-[9px] mb-[9px]" style="${ITEM}"></span>` +
      `</div>`;
    document.body.appendChild(host);
    mounted.push(host);

    const spaced = getComputedStyle(host.querySelectorAll("span")[1]);
    expect(spaced.marginTop).toBe("3px");
    expect(spaced.marginBottom).toBe("9px");
  });
});
