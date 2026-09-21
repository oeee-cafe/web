import { describe, expect, it } from "vitest";
import { getZoomLevels } from "./useZoomControls";

describe("the zoom ladder", () => {
  const levels = getZoomLevels();

  /*
   * NEO's ZoomPlusCommand only ever adds one. A fractional step above 1x
   * makes some artwork pixels one screen pixel wide and some two, which is
   * what a one pixel line looks like when it wobbles.
   */
  it("has only whole numbers from 1x up, as NEO does", () => {
    expect(levels.filter((level) => level >= 1)).toEqual([1, 2, 3, 4]);
  });

  it("keeps a finer ladder below 1x for fitting a large drawing", () => {
    const below = levels.filter((level) => level < 1);
    expect(below[0]).toBe(0.5);
    expect(below.length).toBeGreaterThan(4);
    expect(below.some((level) => Math.abs(level - 2 / 3) < 1e-9)).toBe(true);
  });

  it("climbs strictly, so no two notches of the wheel land on one zoom", () => {
    for (let i = 1; i < levels.length; i++) {
      expect(levels[i]).toBeGreaterThan(levels[i - 1]);
    }
  });
});
