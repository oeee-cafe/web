import { describe, expect, it } from "vitest";
import { compare, type Pixels } from "./compare";

const picture = (width: number, height: number, fill: number[]): Pixels => {
  const data = new Uint8ClampedArray(width * height * 4);
  for (let at = 0; at < data.length; at += 4) data.set(fill, at);
  return { width, height, data };
};

describe("comparing a replay with what was saved", () => {
  it("finds two identical pictures the same", () => {
    const result = compare(picture(3, 2, [10, 20, 30, 255]), picture(3, 2, [10, 20, 30, 255]));
    expect(result).toMatchObject({ sameSize: true, differing: 0, total: 6 });
  });

  /** No tolerance: the claim is that a replay renders what was drawn, and a
   * pixel one step off is a replay that does not. */
  it("counts a pixel one step off in one channel", () => {
    const saved = picture(2, 2, [10, 20, 30, 255]);
    const replay = picture(2, 2, [10, 20, 30, 255]);
    replay.data[4 + 2] = 31;
    const result = compare(replay, saved);
    expect(result.differing).toBe(1);
    expect(Array.from(result.mask.slice(4, 8))).toEqual([255, 0, 0, 255]);
    expect(Array.from(result.mask.slice(0, 4))).toEqual([0, 0, 0, 0]);
  });

  it("treats nothing as nothing, whatever its colour channels hold", () => {
    expect(compare(picture(1, 1, [0, 0, 0, 0]), picture(1, 1, [9, 9, 9, 0])).differing).toBe(0);
  });

  it("calls pictures of different sizes wholly different", () => {
    const result = compare(picture(2, 2, [0, 0, 0, 255]), picture(3, 2, [0, 0, 0, 255]));
    expect(result).toMatchObject({ sameSize: false, differing: 6, total: 6 });
  });
});
