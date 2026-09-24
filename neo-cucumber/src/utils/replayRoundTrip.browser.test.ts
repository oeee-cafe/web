import { describe, expect, it } from "vitest";
import { DrawingEngine } from "../DrawingEngine";
import { ActionRecorder } from "./ActionRecorder";
import {
  LAYER,
  LINETYPE,
  createCanonicalPainter,
  decodePCH,
  describeDifference,
  firstPixelDifference,
  readPixels,
  replayWithNeo,
} from "../test/neoHarness";

const W = 64;
const H = 64;

interface Stroke {
  layer: number;
  color: [number, number, number, number];
  size: number;
  points: [number, number][];
}

/**
 * Drives a drawing exactly as the offline app does -- the same engine calls
 * from useBaseDrawing.performDrawing and the same recorder frames from
 * usePainterDrawing -- so the replay under test is the one users would upload.
 */
class OfflineSession {
  readonly engine = new DrawingEngine(W, H);
  readonly recorder = new ActionRecorder();

  private layerBuffer(layer: number) {
    return layer === LAYER.FOREGROUND
      ? this.engine.layers.foreground
      : this.engine.layers.background;
  }

  fill(layer: number, x: number, y: number, color: [number, number, number, number]) {
    const [r, g, b, a] = color;
    this.engine.doFloodFill(this.layerBuffer(layer), x, y, r, g, b, a);
    this.recorder.step();
    this.recorder.push(
      "floodFill",
      layer,
      x,
      y,
      (a << 24) | (b << 16) | (g << 8) | r
    );
  }

  stroke({ layer, color, size, points }: Stroke) {
    const buffer = this.layerBuffer(layer);
    const [r, g, b, a] = color;

    // pointer down -> onDrawPoint: dot, plus the full frame header
    const [x0, y0] = points[0];
    this.recorder.step();
    this.recorder.push(
      "freeHand", layer, r, g, b, a, 0, 0, 0, size, 0, LINETYPE.PEN,
      x0, y0, x0, y0
    );
    this.engine.drawLine(buffer, x0, y0, x0, y0, size, "solid", r, g, b, a);

    // pointer move -> onDrawLine: new point appended, segment drawn new->prev
    for (let i = 1; i < points.length; i++) {
      const [px, py] = points[i - 1];
      const [nx, ny] = points[i];
      this.recorder.push(nx, ny);
      this.engine.drawLine(buffer, nx, ny, px, py, size, "solid", r, g, b, a);
    }

    // pointer up -> cleanupPointerState clears the joint-dedup state
    this.engine.setStrokeState(null);
  }

  undo() {
    this.recorder.back();
  }

  redo() {
    this.recorder.forward();
  }
}

async function replayThroughNeo(recorder: ActionRecorder) {
  const decoded = await decodePCH(recorder.getReplayBlob(W, H));
  expect(decoded.magic).toBe("NEO ");
  expect(decoded.width).toBe(W);
  expect(decoded.height).toBe(H);

  const cp = createCanonicalPainter(W, H);
  replayWithNeo(cp, decoded.items);
  return {
    background: readPixels(cp.contexts[LAYER.BACKGROUND], W, H),
    foreground: readPixels(cp.contexts[LAYER.FOREGROUND], W, H),
    items: decoded.items,
  };
}

function expectReplayMatchesCanvas(
  session: OfflineSession,
  replayed: { background: Uint8ClampedArray; foreground: Uint8ClampedArray }
) {
  const ourBg = new Uint8ClampedArray(session.engine.layers.background);
  const ourFg = new Uint8ClampedArray(session.engine.layers.foreground);

  expect(
    firstPixelDifference(ourBg, replayed.background),
    `background: ${describeDifference(ourBg, replayed.background, W)}`
  ).toBe(-1);
  expect(
    firstPixelDifference(ourFg, replayed.foreground),
    `foreground: ${describeDifference(ourFg, replayed.foreground, W)}`
  ).toBe(-1);
}

const BLACK: [number, number, number, number] = [0, 0, 0, 255];
const MAROON: [number, number, number, number] = [128, 0, 0, 255];
const CREAM: [number, number, number, number] = [240, 224, 214, 255];

describe("replay round trip through canonical NEO", () => {
  it("reproduces a single stroke", async () => {
    const s = new OfflineSession();
    s.stroke({
      layer: LAYER.BACKGROUND,
      color: BLACK,
      size: 3,
      points: [
        [10, 10],
        [24, 19],
        [40, 44],
        [55, 50],
      ],
    });

    expectReplayMatchesCanvas(s, await replayThroughNeo(s.recorder));
  });

  it("reproduces a two-tone session: fill, strokes in both pen colors", async () => {
    const s = new OfflineSession();
    s.fill(LAYER.BACKGROUND, 0, 0, CREAM);
    s.stroke({
      layer: LAYER.BACKGROUND,
      color: MAROON,
      size: 2,
      points: [
        [8, 8],
        [20, 30],
        [35, 33],
        [50, 55],
      ],
    });
    // "Erasing" is just the background color, as in Tegaki
    s.stroke({
      layer: LAYER.BACKGROUND,
      color: CREAM,
      size: 10,
      points: [
        [30, 30],
        [40, 40],
      ],
    });

    expectReplayMatchesCanvas(s, await replayThroughNeo(s.recorder));
  });

  it("reproduces strokes across both layers", async () => {
    const s = new OfflineSession();
    s.stroke({
      layer: LAYER.BACKGROUND,
      color: MAROON,
      size: 5,
      points: [
        [5, 30],
        [58, 34],
      ],
    });
    s.stroke({
      layer: LAYER.FOREGROUND,
      color: BLACK,
      size: 2,
      points: [
        [30, 5],
        [34, 58],
      ],
    });

    expectReplayMatchesCanvas(s, await replayThroughNeo(s.recorder));
  });

  it("reproduces consecutive strokes that share an endpoint", async () => {
    const s = new OfflineSession();
    s.stroke({
      layer: LAYER.BACKGROUND,
      color: BLACK,
      size: 4,
      points: [
        [12, 12],
        [30, 31],
      ],
    });
    s.stroke({
      layer: LAYER.BACKGROUND,
      color: BLACK,
      size: 4,
      points: [
        [30, 31],
        [52, 14],
      ],
    });

    expectReplayMatchesCanvas(s, await replayThroughNeo(s.recorder));
  });

  it("reproduces single-tap strokes", async () => {
    const s = new OfflineSession();
    s.stroke({ layer: LAYER.BACKGROUND, color: BLACK, size: 7, points: [[20, 20]] });
    s.stroke({ layer: LAYER.BACKGROUND, color: BLACK, size: 7, points: [[20, 20]] });
    s.stroke({ layer: LAYER.BACKGROUND, color: MAROON, size: 1, points: [[44, 44]] });

    expectReplayMatchesCanvas(s, await replayThroughNeo(s.recorder));
  });

  it("reproduces a long many-segment stroke", async () => {
    const points: [number, number][] = [];
    for (let i = 0; i < 60; i++) {
      points.push([
        Math.round(4 + i),
        Math.round(32 + 24 * Math.sin(i / 6)),
      ]);
    }

    const s = new OfflineSession();
    s.stroke({ layer: LAYER.BACKGROUND, color: BLACK, size: 3, points });

    expectReplayMatchesCanvas(s, await replayThroughNeo(s.recorder));
  });

  it("reproduces the canvas after an undo", async () => {
    const s = new OfflineSession();
    s.stroke({
      layer: LAYER.BACKGROUND,
      color: BLACK,
      size: 3,
      points: [
        [10, 10],
        [30, 24],
      ],
    });

    // Snapshot the state the user will be left with after undoing the next stroke
    const expectedBg = new Uint8ClampedArray(s.engine.layers.background);

    s.stroke({
      layer: LAYER.BACKGROUND,
      color: MAROON,
      size: 6,
      points: [
        [40, 40],
        [55, 12],
      ],
    });
    s.undo();

    const replayed = await replayThroughNeo(s.recorder);
    expect(
      firstPixelDifference(expectedBg, replayed.background),
      describeDifference(expectedBg, replayed.background, W)
    ).toBe(-1);
  });

  it("reproduces the canvas after undo then redo", async () => {
    const s = new OfflineSession();
    s.stroke({
      layer: LAYER.BACKGROUND,
      color: BLACK,
      size: 3,
      points: [
        [10, 10],
        [30, 24],
      ],
    });
    s.stroke({
      layer: LAYER.BACKGROUND,
      color: MAROON,
      size: 6,
      points: [
        [40, 40],
        [55, 12],
      ],
    });
    s.undo();
    s.redo();

    expectReplayMatchesCanvas(s, await replayThroughNeo(s.recorder));
  });

  it("reproduces the canvas when a stroke is drawn after an undo", async () => {
    const s = new OfflineSession();
    s.stroke({
      layer: LAYER.BACKGROUND, color: BLACK, size: 3,
      points: [[10, 10], [30, 24]],
    });
    s.stroke({
      layer: LAYER.BACKGROUND, color: MAROON, size: 6,
      points: [[40, 40], [55, 12]],
    });
    s.undo();

    // Recording a new stroke discards the undone one (ActionRecorder.step)
    s.stroke({
      layer: LAYER.BACKGROUND, color: MAROON, size: 2,
      points: [[50, 50], [58, 20]],
    });

    const replayed = await replayThroughNeo(s.recorder);
    expect(replayed.items).toHaveLength(2);
    // Rebuild what the canvas should be: first stroke plus the replacement
    const rebuilt = new OfflineSession();
    rebuilt.stroke({
      layer: LAYER.BACKGROUND, color: BLACK, size: 3,
      points: [[10, 10], [30, 24]],
    });
    rebuilt.stroke({
      layer: LAYER.BACKGROUND, color: MAROON, size: 2,
      points: [[50, 50], [58, 20]],
    });
    const expected = new Uint8ClampedArray(rebuilt.engine.layers.background);
    expect(
      firstPixelDifference(expected, replayed.background),
      describeDifference(expected, replayed.background, W)
    ).toBe(-1);
  });

  it("survives a restore frame appended after an undo", async () => {
    const s = new OfflineSession();
    s.stroke({
      layer: LAYER.BACKGROUND, color: BLACK, size: 3,
      points: [[10, 10], [30, 24]],
    });
    const expectedBg = new Uint8ClampedArray(s.engine.layers.background);

    s.stroke({
      layer: LAYER.BACKGROUND, color: MAROON, size: 6,
      points: [[40, 40], [55, 12]],
    });
    s.undo();
    s.recorder.addRestoreAction("data:image/png;base64,BG", "data:image/png;base64,FG");

    const replayed = await replayThroughNeo(s.recorder);
    // The restore frame must survive, and the undone stroke must not
    expect(replayed.items[replayed.items.length - 1][0]).toBe("restore");
    expect(
      replayed.items.filter((i) => i[0] === "freeHand")
    ).toHaveLength(1);
    expect(
      firstPixelDifference(expectedBg, replayed.background),
      describeDifference(expectedBg, replayed.background, W)
    ).toBe(-1);
  });
});

describe("brush types the engine gained from the unified kernels", () => {
  // The .pch format carries lineType directly, so an offline drawing can use
  // every type NEO has without touching the collaborative wire format.
  const TYPES: [string, number][] = [
    ["solid", LINETYPE.PEN],
    ["eraser", LINETYPE.ERASER],
    ["brush", LINETYPE.BRUSH],
    ["halftone", LINETYPE.TONE],
    ["dodge", LINETYPE.DODGE],
    ["burn", LINETYPE.BURN],
    ["blur", LINETYPE.BLUR],
  ];

  for (const [brushType, lineType] of TYPES) {
    it(`records ${brushType} as line type ${lineType} and replays it`, async () => {
      const engine = new DrawingEngine(W, H);
      const cp = createCanonicalPainter(W, H);

      // Something to act on: dodge, burn, blur and the eraser need pixels
      const underlay: [number, number][] = [
        [4, 30],
        [58, 30],
      ];
      for (const target of ["ours", "neo"] as const) {
        const points = underlay;
        if (target === "ours") {
          engine.drawLine(
            engine.layers.background, points[0][0], points[0][1],
            points[0][0], points[0][1], 20, "solid", 120, 160, 200, 255
          );
          engine.drawLine(
            engine.layers.background, points[1][0], points[1][1],
            points[0][0], points[0][1], 20, "solid", 120, 160, 200, 255
          );
          engine.setStrokeState(null);
        } else {
          cp.painter._currentColor = [120, 160, 200, 255];
          cp.painter._currentWidth = 20;
          cp.painter.drawLine(cp.contexts[0], points[0][0], points[0][1], points[0][0], points[0][1], LINETYPE.PEN);
          cp.painter.drawLine(cp.contexts[0], points[1][0], points[1][1], points[0][0], points[0][1], LINETYPE.PEN);
          cp.painter.prevLine = null;
        }
      }

      const stroke: [number, number][] = [
        [12, 14],
        [32, 30],
        [50, 44],
      ];

      // Ours, through the painter's public entry point
      for (let i = 0; i < stroke.length; i++) {
        const [x, y] = stroke[i];
        const [px, py] = i === 0 ? stroke[0] : stroke[i - 1];
        engine.drawLine(
          engine.layers.background, x, y, px, py, 9, brushType, 20, 20, 20, 255
        );
      }
      engine.setStrokeState(null);

      // Canonical NEO, driven by the line type the recorder would write
      cp.painter._currentColor = [20, 20, 20, 255];
      cp.painter._currentWidth = 9;
      for (let i = 0; i < stroke.length; i++) {
        const [x, y] = stroke[i];
        const [px, py] = i === 0 ? stroke[0] : stroke[i - 1];
        cp.painter.drawLine(cp.contexts[0], x, y, px, py, lineType);
      }
      cp.painter.prevLine = null;

      // Canvas getImageData premultiplies, so a fully erased pixel loses its
      // colour there but keeps it in a buffer. Unobservable either way.
      const zeroTransparent = (px: Uint8ClampedArray) => {
        const out = new Uint8ClampedArray(px);
        for (let i = 0; i < out.length; i += 4) {
          if (out[i + 3] === 0) {
            out[i] = 0;
            out[i + 1] = 0;
            out[i + 2] = 0;
          }
        }
        return out;
      };
      const ours = zeroTransparent(new Uint8ClampedArray(engine.layers.background));
      const neo = zeroTransparent(readPixels(cp.contexts[0], W, H));
      expect(
        firstPixelDifference(ours, neo),
        `${brushType}: ${describeDifference(ours, neo, W)}`
      ).toBe(-1);
    });
  }
});
