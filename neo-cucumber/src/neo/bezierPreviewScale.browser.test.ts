import { describe, expect, it } from "vitest";
import { drawBezierPreview, type Backdrop } from "./regionPreview";

/**
 * The bezier preview at a zoom other than 1x.
 *
 * It used to scale the brush width by the zoom and hand that to the pen
 * kernel, whose round table is indexed by whole widths from 1 to 30. A
 * fit-to-screen zoom -- which is what every tablet opens at -- made the
 * width fractional, and a wide brush at 2x made it larger than 30; either
 * way the lookup was `undefined` and the preview threw. It threw out of the
 * painter's release handler, which is what left the pointer latched and the
 * canvas dead (useBaseDrawing.browser tests cover that half).
 *
 * NEO rasterises the curve at the artwork's size and scales the result, so
 * that is what these pin: no throw at any rung of the zoom ladder, and the
 * curve as thick on screen as the artwork's pixels are.
 */

const W = 80;
const H = 40;
const CURVE: [number, number, number, number] = [73, 19, 211, 96];

function overlayAt(scale: number): CanvasRenderingContext2D {
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(W * scale));
  canvas.height = Math.max(1, Math.round(H * scale));
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) throw new Error("no 2d context");
  return ctx;
}

function whiteBackdrop(scale: number): Backdrop {
  const layer = new Uint8ClampedArray(W * H * 4).fill(255);
  return { width: W, height: H, scale, layers: [layer] };
}

/** A curve whose every point sits on y=20: on screen it is a horizontal bar. */
const FLAT = [10, 20, 30, 20, 30, 20, 60, 20];

/** Rows of `column` painted with the curve colour, in the overlay's pixels. */
function curveRows(ctx: CanvasRenderingContext2D, column: number): number[] {
  const rows: number[] = [];
  const { data } = ctx.getImageData(column, 0, 1, ctx.canvas.height);
  for (let y = 0; y < ctx.canvas.height; y++) {
    const i = y * 4;
    if (
      data[i] === CURVE[0] &&
      data[i + 1] === CURVE[1] &&
      data[i + 2] === CURVE[2] &&
      data[i + 3] === 255
    ) {
      rows.push(y);
    }
  }
  return rows;
}

describe("the bezier preview away from 1x", () => {
  it("survives every rung of the zoom ladder, and the widest brush at 2x", () => {
    for (const scale of [0.25, 0.33, 0.5, 0.75, 1.5, 2, 3, 4]) {
      const ctx = overlayAt(scale);
      expect(() =>
        drawBezierPreview(ctx, FLAT, whiteBackdrop(scale), 1, { color: CURVE, width: 7 }),
      ).not.toThrow();
      expect(curveRows(ctx, Math.round(45 * scale)).length).toBeGreaterThan(0);
    }

    const ctx = overlayAt(2);
    expect(() =>
      drawBezierPreview(ctx, FLAT, whiteBackdrop(2), 2, { color: CURVE, width: 30 }),
    ).not.toThrow();
  });

  it("is as thick on screen as the artwork's own pixels, magnified", () => {
    // Width 4 at y=20 covers rows 18..21 of the artwork (r0i = 2). At 2x
    // that is rows 36..43 of the display: eight rows, not the four a 4px
    // kernel drawn straight onto the display would give.
    const ctx = overlayAt(2);
    drawBezierPreview(ctx, FLAT, whiteBackdrop(2), 1, { color: CURVE, width: 4 });

    // x=70 is clear of the handle rings at 20, 60 and 120.
    expect(curveRows(ctx, 70)).toEqual([36, 37, 38, 39, 40, 41, 42, 43]);
  });

  it("shrinks with the artwork when zoomed out", () => {
    // Width 7 covers rows 17..23 of the artwork; at half size that is three
    // or four display rows somewhere in 8..11, never the seven a 7px kernel
    // would paint on the display.
    const ctx = overlayAt(0.5);
    drawBezierPreview(ctx, FLAT, whiteBackdrop(0.5), 1, { color: CURVE, width: 7 });

    const rows = curveRows(ctx, 22);
    expect(rows.length).toBeGreaterThanOrEqual(3);
    expect(rows.length).toBeLessThanOrEqual(4);
    for (const row of rows) {
      expect(row).toBeGreaterThanOrEqual(8);
      expect(row).toBeLessThanOrEqual(11);
    }
  });
});
