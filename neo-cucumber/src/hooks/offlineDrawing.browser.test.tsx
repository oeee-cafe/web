import { describe, expect, it } from "vitest";
import { act, useEffect, useRef } from "react";
import { createRoot } from "react-dom/client";
import { usePainterDrawing } from "./usePainterDrawing";
import type { DrawingState } from "../types/drawing";
import {
  LAYER,
  createCanonicalPainter,
  decodePCH,
  describeDifference,
  firstPixelDifference,
  readPixels,
  replayWithNeo,
} from "../test/neoHarness";
import { NeoReplay } from "../neo/NeoReplay";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const W = 80;
const H = 60;
// Moves used to have to be spaced out because useBaseDrawing throttled
// pointermove to one every 12ms. It takes every sample now, so this only has
// to yield far enough for React to flush between them.
const MOVE_INTERVAL_MS = 0;

type OfflineApi = ReturnType<typeof usePainterDrawing>;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function drawingState(overrides: Partial<DrawingState> = {}): DrawingState {
  return {
    brushSize: 3,
    opacity: 255,
    color: "#800000",
    brushType: "solid",
    layerType: "background",
    zoomLevel: 100,
    fgVisible: true,
    bgVisible: true,
    isFlippedHorizontal: false,
    ...overrides,
  };
}

/**
 * Mounts the real offline drawing stack -- the same hook the offline app uses,
 * wired to a real canvas -- so pointer events exercise the production call
 * path rather than a restatement of it.
 */
async function mountOfflineDrawing(state: DrawingState) {
  const captured: { api: OfflineApi | null } = { api: null };
  let current = state;

  function Harness() {
    const appRef = useRef<HTMLDivElement>(null);
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const api = usePainterDrawing({
      canvasRef, appRef, drawingState: current,
      zoomLevel: 100, canvasWidth: W, canvasHeight: H,
      mode: { kind: "offline" },
    });
    useEffect(() => {
      captured.api = api;
    });
    return (
      <div id="app" ref={appRef}>
        <canvas
          id="canvas"
          ref={canvasRef}
          width={W}
          height={H}
          style={{ width: `${W}px`, height: `${H}px`, display: "block" }}
        />
      </div>
    );
  }

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(async () => {
    root.render(<Harness />);
  });

  // The engine is created in an effect and exposed through a ref, so a
  // re-render is needed before the hook's return value reflects it.
  for (let i = 0; i < 50 && !captured.api?.drawingEngine; i++) {
    await act(async () => {
      await sleep(10);
      root.render(<Harness />);
    });
  }
  const api = captured.api;
  if (!api?.drawingEngine) throw new Error("drawing engine never initialised");

  const canvas = container.querySelector("#canvas") as HTMLCanvasElement;
  const rect = canvas.getBoundingClientRect();

  const send = async (type: string, x: number, y: number) => {
    await act(async () => {
      canvas.dispatchEvent(
        new PointerEvent(type, {
          pointerId: 1,
          pointerType: "mouse",
          button: 0,
          buttons: type === "pointerup" ? 0 : 1,
          clientX: rect.left + x,
          clientY: rect.top + y,
          bubbles: true,
          cancelable: true,
        })
      );
    });
  };

  /** Presses, moves through each point, and releases. */
  const strokeThrough = async (points: [number, number][]) => {
    await send("pointerdown", points[0][0], points[0][1]);
    for (let i = 1; i < points.length; i++) {
      await act(async () => {
        await sleep(MOVE_INTERVAL_MS);
      });
      await send("pointermove", points[i][0], points[i][1]);
    }
    await send("pointerup", points[points.length - 1][0], points[points.length - 1][1]);
  };

  /** Mimics the toolbox/hotkeys changing drawing state, including mid-stroke. */
  const updateDrawingState = async (patch: Partial<DrawingState>) => {
    current = { ...current, ...patch };
    await act(async () => {
      root.render(<Harness />);
    });
  };

  const press = (x: number, y: number) => send("pointerdown", x, y);
  const moveTo = async (x: number, y: number) => {
    await act(async () => {
      await sleep(MOVE_INTERVAL_MS);
    });
    await send("pointermove", x, y);
  };
  const release = (x: number, y: number) => send("pointerup", x, y);

  return {
    api,
    container,
    strokeThrough,
    updateDrawingState,
    press,
    moveTo,
    release,
  };
}

async function replayThroughNeo(api: OfflineApi) {
  const decoded = await decodePCH(api.replay!.getReplayBlob());
  const cp = createCanonicalPainter(W, H);
  replayWithNeo(cp, decoded.items);
  return {
    background: readPixels(cp.contexts[LAYER.BACKGROUND], W, H),
    foreground: readPixels(cp.contexts[LAYER.FOREGROUND], W, H),
    items: decoded.items,
  };
}

function expectMatch(api: OfflineApi, replayed: { background: Uint8ClampedArray }) {
  const engine = api.drawingEngine!;
  const ours = new Uint8ClampedArray(engine.layers.background);
  expect(
    firstPixelDifference(ours, replayed.background),
    describeDifference(ours, replayed.background, W)
  ).toBe(-1);
}

describe("offline drawing end to end", () => {
  it("records a replay that NEO renders back to the live canvas", async () => {
    const { api, strokeThrough } = await mountOfflineDrawing(drawingState());

    await strokeThrough([
      [10, 10],
      [24, 18],
      [38, 41],
      [60, 46],
      [70, 20],
    ]);

    const replayed = await replayThroughNeo(api);
    expect(replayed.items.length).toBeGreaterThan(0);
    expect(replayed.items[0][0]).toBe("freeHand");
    expectMatch(api, replayed);
  });

  it("matches after several strokes, including one sharing an endpoint", async () => {
    const { api, strokeThrough } = await mountOfflineDrawing(drawingState());

    await strokeThrough([
      [12, 12],
      [30, 30],
    ]);
    await strokeThrough([
      [30, 30],
      [55, 14],
    ]);
    await strokeThrough([[64, 40]]);

    expectMatch(api, await replayThroughNeo(api));
  });

  it("matches after an undo", async () => {
    const { api, strokeThrough } = await mountOfflineDrawing(drawingState());

    await strokeThrough([
      [8, 8],
      [30, 26],
    ]);
    const expected = new Uint8ClampedArray(api.drawingEngine!.layers.background);

    await strokeThrough([
      [50, 50],
      [70, 20],
    ]);
    await act(async () => {
      api.undo();
    });

    const replayed = await replayThroughNeo(api);
    expect(
      firstPixelDifference(expected, replayed.background),
      describeDifference(expected, replayed.background, W)
    ).toBe(-1);
    expectMatch(api, replayed);
  });

  it("matches when the pen size is changed mid-stroke", async () => {
    const { api, updateDrawingState, press, moveTo, release } =
      await mountOfflineDrawing(drawingState({ brushSize: 3 }));

    await press(10, 10);
    await moveTo(30, 22);
    // The [ / ] hotkeys land here: drawing state changes while the pointer
    // is still down.
    await updateDrawingState({ brushSize: 14 });
    await moveTo(55, 40);
    await release(55, 40);

    expectMatch(api, await replayThroughNeo(api));
  });

  /**
   * A canvas cannot keep a colour under zero alpha, so in NEO a pixel the
   * eraser has cleared reads back as 0x00000000 -- the same as one nothing
   * was ever drawn on -- and a flood beside it runs straight through. A
   * buffer that kept the stroke's red under the cleared alpha made the fill
   * stop at an invisible wall, and the replay, run by NEO, did not.
   */
  it("matches when a fill crosses ground the eraser cleared", async () => {
    const { api, strokeThrough, updateDrawingState } =
      await mountOfflineDrawing(drawingState({ brushSize: 4, color: "#c80000" }));

    await strokeThrough([
      [4, 30],
      [76, 30],
    ]);
    await updateDrawingState({ brushType: "eraser", brushSize: 12 });
    await strokeThrough([
      [2, 30],
      [78, 30],
    ]);
    await updateDrawingState({ brushType: "fill", color: "#0000ff" });
    await strokeThrough([[40, 8]]);

    const layer = api.drawingEngine!.layers.background;
    // On the line the eraser cleared.
    const cleared = (30 * W + 40) * 4;
    expect(Array.from(layer.subarray(cleared, cleared + 4))).toEqual([0, 0, 255, 255]);
    expectMatch(api, await replayThroughNeo(api));
  });

  it("matches when the pen color is changed mid-stroke", async () => {
    const { api, updateDrawingState, press, moveTo, release } =
      await mountOfflineDrawing(drawingState({ color: "#800000" }));

    await press(10, 45);
    await moveTo(32, 30);
    await updateDrawingState({ color: "#313768" });
    await moveTo(66, 14);
    await release(66, 14);

    expectMatch(api, await replayThroughNeo(api));
  });

  it("matches with a restore frame appended, as saving does", async () => {
    const { api, strokeThrough } = await mountOfflineDrawing(drawingState());

    await strokeThrough([
      [10, 40],
      [40, 12],
      [70, 44],
    ]);
    await act(async () => {
      api.replay!.addRestoreAction(api.drawingEngine!);
    });

    const replayed = await replayThroughNeo(api);
    expect(replayed.items[replayed.items.length - 1][0]).toBe("restore");
    expectMatch(api, replayed);
  });

  it("records a continued image as an opening restore", async () => {
    const source = document.createElement("canvas");
    source.width = W;
    source.height = H;
    const context = source.getContext("2d")!;
    context.fillStyle = "#317842";
    context.fillRect(0, 0, W, H);

    const { api, strokeThrough } = await mountOfflineDrawing(drawingState());
    await act(async () => {
      await api.initializeFromImage(source.toDataURL("image/png"));
    });
    await strokeThrough([
      [10, 10],
      [60, 40],
    ]);

    const decoded = await decodePCH(api.replay!.getReplayBlob());
    const replay = new NeoReplay(W, H);
    await replay.playAll(decoded.items);
    const replayed = {
      background: replay.getLayerPixels(LAYER.BACKGROUND),
      items: decoded.items,
    };
    expect(replayed.items[0][0]).toBe("restore");
    expect(replayed.items[1][0]).toBe("freeHand");
    expectMatch(api, replayed);
  });
});

/**
 * Continuing a saved drawing (`resumeFromReplay`), as a draft is continued:
 * one mount draws and saves, a second -- a new page, days later -- opens the
 * file and draws on, through real pointer events both times. The file it
 * saves has to be the whole drawing, and NEO has to play it to what the
 * second canvas shows.
 *
 * The strokes NEO is held to are opaque, as everywhere in this file: the
 * painter keeps a translucent pixel more exactly than NEO's canvas can
 * (`PixelSurface`), so on a translucent one only the restore frames agree.
 * The translucent strokes here are what the resumed canvas is held to, bit
 * for bit, against the one that was saved.
 */
describe("resuming a saved drawing", () => {
  /** What a save writes: the closing restore, then the file. */
  async function saved(api: OfflineApi): Promise<ArrayBuffer> {
    await act(async () => {
      api.replay!.addRestoreAction(api.drawingEngine!);
    });
    return api.replay!.getReplayBlob().arrayBuffer();
  }

  function layersOf(api: OfflineApi) {
    const engine = api.drawingEngine!;
    return {
      background: new Uint8ClampedArray(engine.layers.background),
      foreground: new Uint8ClampedArray(engine.layers.foreground),
    };
  }

  function expectSameLayer(ours: Uint8ClampedArray, theirs: Uint8ClampedArray) {
    expect(firstPixelDifference(ours, theirs), describeDifference(ours, theirs, W)).toBe(-1);
  }

  function expectSameLayers(
    ours: { background: Uint8ClampedArray; foreground: Uint8ClampedArray },
    theirs: { background: Uint8ClampedArray; foreground: Uint8ClampedArray },
  ) {
    expectSameLayer(ours.background, theirs.background);
    expectSameLayer(ours.foreground, theirs.foreground);
  }

  /** The file played as a viewer plays it: restores and all, in order. */
  async function playedWhole(file: ArrayBuffer) {
    const decoded = await decodePCH(new Blob([file]));
    const neo = new NeoReplay(W, H);
    await neo.playAll(decoded.items);
    return {
      background: neo.getLayerPixels(LAYER.BACKGROUND),
      foreground: neo.getLayerPixels(LAYER.FOREGROUND),
      items: decoded.items,
    };
  }

  async function resumed(file: ArrayBuffer, state = drawingState()) {
    const sitting = await mountOfflineDrawing(state);
    await act(async () => {
      await sitting.api.resumeFromReplay(file);
    });
    return sitting;
  }

  /** The first sitting: an opaque stroke behind, a translucent one in front. */
  async function firstSitting() {
    const first = await mountOfflineDrawing(drawingState());
    await first.strokeThrough([
      [8, 10],
      [30, 40],
      [52, 12],
    ]);
    await first.updateDrawingState({
      layerType: "foreground",
      brushType: "brush",
      brushSize: 9,
      color: "#2050c0",
      opacity: 128,
    });
    await first.strokeThrough([
      [10, 30],
      [70, 34],
    ]);
    const file = await saved(first.api);
    return { first, file, layers: layersOf(first.api) };
  }

  it("opens on exactly the canvas that was saved", async () => {
    const { file, layers } = await firstSitting();
    const second = await resumed(file);
    expectSameLayers(layersOf(second.api), layers);
  });

  it("saves the whole drawing, which NEO renders to the canvas it was drawn on", async () => {
    const { file, layers } = await firstSitting();

    const second = await resumed(file, drawingState({ color: "#106020" }));
    await second.strokeThrough([
      [20, 50],
      [44, 6],
      [76, 52],
    ]);
    await second.updateDrawingState({ brushType: "fill", color: "#e0c040" });
    await second.strokeThrough([[70, 8]]);
    const whole = await saved(second.api);
    const ours = layersOf(second.api);

    const played = await playedWhole(whole);
    // The first sitting's strokes and the restore that closed it, then the
    // second's and the restore that closes this one: NEO's continue, which
    // keeps the frames it loaded and adds to them.
    expect(played.items.map((item) => item[0])).toEqual([
      "freeHand",
      "freeHand",
      "restore",
      "freeHand",
      "floodFill",
      "restore",
    ]);
    expectSameLayer(ours.background, played.background);
    // NEO itself, drawing every stroke of both sittings with the restores
    // passed over, arrives at the same background.
    expectSameLayer(ours.background, (await replayThroughNeo(second.api)).background);
    // Nothing was drawn in front, which is still the first sitting's.
    expectSameLayer(ours.foreground, layers.foreground);
  });

  it("comes back exactly from a second save, translucent strokes and all", async () => {
    const { file } = await firstSitting();

    const second = await resumed(
      file,
      drawingState({
        layerType: "foreground",
        brushType: "brush",
        brushSize: 7,
        color: "#c03010",
        opacity: 100,
      }),
    );
    // Across the first sitting's translucent stroke, so the two blend.
    await second.strokeThrough([
      [40, 4],
      [40, 56],
    ]);
    const again = await saved(second.api);
    const ours = layersOf(second.api);

    const third = await resumed(again);
    expectSameLayers(layersOf(third.api), ours);
  });

  it("counts the strokes of both sittings", async () => {
    const { file } = await firstSitting();

    const second = await resumed(file);
    expect(second.api.replay!.strokeCount()).toBe(2);
    await second.strokeThrough([
      [20, 20],
      [60, 20],
    ]);
    await saved(second.api);
    expect(second.api.replay!.strokeCount()).toBe(3);
  });

  it("undoes back to where it was resumed, and no further", async () => {
    const { file, layers } = await firstSitting();

    const second = await resumed(file);
    await second.strokeThrough([
      [6, 6],
      [74, 54],
    ]);
    // One undo for the stroke, and two more that have nothing to take.
    await act(async () => {
      second.api.undo();
      second.api.undo();
      second.api.undo();
    });
    expectSameLayers(layersOf(second.api), layers);

    const played = await playedWhole(await saved(second.api));
    // Saving again with nothing new replaces the closing restore rather than
    // adding a second one.
    expect(played.items.map((item) => item[0])).toEqual(["freeHand", "freeHand", "restore"]);
    expectSameLayer(layers.background, (await replayThroughNeo(second.api)).background);
  });

  it("refuses a replay of another size", async () => {
    const { file } = await firstSitting();
    const bytes = new Uint8Array(file.slice(0));
    bytes[4] = W + 1;

    const second = await mountOfflineDrawing(drawingState());
    let error: unknown = null;
    await act(async () => {
      await second.api.resumeFromReplay(bytes.buffer).catch((e: unknown) => {
        error = e;
      });
    });
    expect(String(error)).toMatch(/canvas/);
  });
});
