/**
 * POTI-board's palette effects: Bright, Dark and Invert.
 *
 * Its `P_Effect(a)` moves every channel of every swatch by `a` and clamps the
 * result, except that `a == 255` flips the sign of the channel instead, which
 * is how one function also inverts. Every swatch changes, whichever is
 * selected, and it goes through `setColors`, so the drawing colour does not.
 */

export type PaletteEffect = "bright" | "dark" | "invert";

/** POTI's argument for each button: `P_Effect(10)`, `(-10)` and `(255)`. */
const AMOUNTS: Record<PaletteEffect, number> = {
  bright: 10,
  dark: -10,
  invert: 255,
};

const channel = (value: number) =>
  Math.max(0, Math.min(255, value)).toString(16).padStart(2, "0");

/** Applies one effect to a palette of `#rrggbb` colours, in any order. */
export function applyPaletteEffect(
  colors: readonly string[],
  effect: PaletteEffect,
): string[] {
  const amount = AMOUNTS[effect];
  const sign = amount === 255 ? -1 : 1;
  return colors.map((color) => {
    const rgb = [1, 3, 5].map((i) => parseInt(color.slice(i, i + 2), 16));
    return `#${rgb.map((value) => channel(amount + value * sign)).join("")}`;
  });
}

/**
 * POTI-board's gradation: fourteen colours stepping from `start` towards
 * `end`, in NEO's order, the order its `ChengeGrad` hands to `setColors`.
 *
 * Kept as POTI computes it, which is not a straight blend: each channel moves
 * by a fifteenth of the distance, truncated, over fourteen colours, so the
 * last one stops short of `end`. What a user of POTI gets from two colours is
 * what they get here.
 *
 * POTI also turns a channel back when it would leave 0-255, but thirteen
 * fifteenths of the way never passes `end`, so that branch never runs -- and
 * nor does the typo in it (`d-=c` for `f-=c`) that would corrupt blue.
 */
export function gradientPalette(start: string, end: string): string[] {
  const channels = (color: string) =>
    [1, 3, 5].map((i) => parseInt(color.slice(i, i + 2), 16));
  const from = channels(start);
  const to = channels(end);
  const step = from.map((value, i) => Math.trunc((value - to[i]) / 15));
  const colors: string[] = [];
  for (let n = 0; n < 14; n++) {
    colors.push(`#${from.map((value, i) => channel(value - step[i] * n)).join("")}`);
  }
  return colors;
}
