import { describe, expect, it } from "vitest";
import { DrawingEngine } from "./DrawingEngine";
import {
  LAYER,
  LINETYPE,
  createCanonicalPainter,
  neoDrawStroke,
  readPixels,
  type StrokeSpec,
} from "./test/neoHarness";

const W = 64;
const H = 64;

function ourDrawStroke(engine: DrawingEngine, stroke: StrokeSpec) {
  const layer = engine.layers.background;
  const [r, g, b, a] = stroke.color;
  const brushType = stroke.lineType === LINETYPE.ERASER ? "eraser" : "solid";
  const [x0, y0] = stroke.points[0];
  engine.drawLine(layer, x0, y0, x0, y0, stroke.width, brushType, r, g, b, a);
  for (let i = 1; i < stroke.points.length; i++) {
    const [px, py] = stroke.points[i - 1];
    const [nx, ny] = stroke.points[i];
    engine.drawLine(layer, nx, ny, px, py, stroke.width, brushType, r, g, b, a);
  }
  engine.setStrokeState(null);
}

function run(strokes: StrokeSpec[], seed: [number, number], fill: [number, number, number, number]) {
  const engine = new DrawingEngine(W, H);
  const cp = createCanonicalPainter(W, H);
  for (const s of strokes) {
    ourDrawStroke(engine, s);
    neoDrawStroke(cp, s);
  }
  const [r, g, b, a] = fill;
  engine.doFloodFill(engine.layers.background, seed[0], seed[1], r, g, b, a);
  cp.painter.doFloodFill(LAYER.BACKGROUND, seed[0], seed[1], (a << 24) | (b << 16) | (g << 8) | r);
  return {
    ours: new Uint8ClampedArray(engine.layers.background),
    neo: readPixels(cp.contexts[LAYER.BACKGROUND], W, H),
  };
}

/**
 * Where the flood reached, every alpha, and the colour of every opaque or
 * cleared pixel must agree exactly. A translucent pixel's colour is the
 * canvas's premultiplied rounding, which the buffer deliberately does not
 * copy (see PixelSurface) and which at alpha 3 is 30 levels.
 */
function differences(ours: Uint8ClampedArray, neo: Uint8ClampedArray): string[] {
  const out: string[] = [];
  for (let i = 0; i < ours.length; i += 4) {
    const a = neo[i + 3];
    const off =
      ours[i + 3] !== a ||
      ((a === 0 || a === 255) && [0, 1, 2].some((c) => ours[i + c] !== neo[i + c]));
    if (off) {
      const p = i / 4;
      out.push(
        `(${p % W}, ${Math.floor(p / W)}): ours [${ours.subarray(i, i + 4)}] vs neo [${neo.subarray(i, i + 4)}]`,
      );
    }
  }
  return out;
}

const line = (color: [number, number, number, number], width: number, lineType: number, points: [number, number][]): StrokeSpec => ({
  layer: LAYER.BACKGROUND,
  color,
  width,
  lineType,
  points,
});

describe("flood fill after the eraser", () => {
  for (const [name, color] of [
    ["black", [0, 0, 0, 255]],
    ["red", [200, 0, 0, 255]],
    ["white", [255, 255, 255, 255]],
  ] as const) {
    for (const eraserAlpha of [255, 128]) {
      it(`${name} line, eraser at ${eraserAlpha}, fill beside it`, () => {
        const { ours, neo } = run(
          [
            line([...color], 6, LINETYPE.PEN, [[4, 32], [60, 32]]),
            line([0, 0, 0, eraserAlpha], 12, LINETYPE.ERASER, [[4, 32], [60, 32]]),
          ],
          [32, 10],
          [0, 0, 255, 255],
        );
        expect(differences(ours, neo).slice(0, 5)).toEqual([]);
      });
    }
  }
});
