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
