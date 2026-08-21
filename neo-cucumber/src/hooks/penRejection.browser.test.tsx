import { beforeEach, describe, expect, it } from "vitest";
import { act, useEffect, useRef } from "react";
import { createRoot } from "react-dom/client";
import { useOfflineDrawing } from "./useOfflineDrawing";
import { resetPenPreference } from "../utils/penPreference";
import type { DrawingState } from "../types/drawing";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const W = 80;
const H = 60;

type OfflineApi = ReturnType<typeof useOfflineDrawing>;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Longer than the hold `useBaseDrawing` puts a finger's press through. */
const PAST_THE_HOLD_MS = 120;

function drawingState(): DrawingState {
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
  };
}

/** Mounts the offline stack so pointer events take the production path. */
async function mountPainter() {
  const captured: { api: OfflineApi | null } = { api: null };
  const state = drawingState();

  function Harness() {
    const appRef = useRef<HTMLDivElement>(null);
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const api = useOfflineDrawing(canvasRef, appRef, state, undefined, 100, W, H);
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

  const send = async (
    type: string,
    x: number,
    y: number,
    pointerId: number,
    pointerType: string
  ) => {
    await act(async () => {
      canvas.dispatchEvent(
        new PointerEvent(type, {
          pointerId,
          pointerType,
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

  /** A whole press-move-release with one pointer of one kind. */
  const strokeWith = async (
    pointerType: string,
    pointerId: number,
    points: [number, number][]
  ) => {
    await send("pointerdown", points[0][0], points[0][1], pointerId, pointerType);
    // A finger's press is held before it becomes a stroke; outlast it, so a
    // touch that is allowed to draw really does.
    if (pointerType === "touch") {
      await act(async () => {
        await sleep(PAST_THE_HOLD_MS);
      });
    }
    for (let i = 1; i < points.length; i++) {
      await send("pointermove", points[i][0], points[i][1], pointerId, pointerType);
    }
    const last = points[points.length - 1];
    await send("pointerup", last[0], last[1], pointerId, pointerType);
  };

  const canvasBytes = () => new Uint8ClampedArray(api.drawingEngine!.layers.background);

  const pixelAt = (bytes: Uint8ClampedArray, x: number, y: number) => {
    const at = (y * W + x) * 4;
    return [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]].join(",");
  };

  const cleanup = () => {
    act(() => root.unmount());
    container.remove();
  };

  return { api, send, strokeWith, canvasBytes, pixelAt, cleanup };
}

function anyPixelChanged(before: Uint8ClampedArray, after: Uint8ClampedArray) {
  for (let i = 0; i < before.length; i++) if (before[i] !== after[i]) return true;
  return false;
}

describe("a session that has seen a pen stops taking fingers", () => {
  // Each case says for itself whether a pen has been seen, rather than
  // inheriting one from the case before it.
  beforeEach(() => resetPenPreference());

  it("draws with a finger when no pen has been used this session", async () => {
    const painter = await mountPainter();
    const before = painter.canvasBytes();

    await painter.strokeWith("touch", 1, [
      [10, 10],
      [30, 30],
    ]);

    expect(anyPixelChanged(before, painter.canvasBytes())).toBe(true);
    painter.cleanup();
  });

  it("drops a palm still inside its hold when the pen lands", async () => {
    const painter = await mountPainter();
    const before = painter.canvasBytes();

    // The hand first, the way it actually happens, and the nib close behind --
    // inside the hold, which is the window the latch cannot cover because this
    // is the first pen of the session.
    await painter.send("pointerdown", 10, 10, 2, "touch");
    await painter.strokeWith("pen", 3, [
      [40, 40],
      [50, 45],
    ]);
    await painter.send("pointerup", 10, 10, 2, "touch");

    const after = painter.canvasBytes();
    expect(painter.pixelAt(after, 10, 10)).toBe(painter.pixelAt(before, 10, 10));
    expect(painter.pixelAt(after, 40, 40)).not.toBe(painter.pixelAt(before, 40, 40));
    painter.cleanup();
  });

  it("ignores a finger once a pen has been seen, and still draws with the pen", async () => {
    const painter = await mountPainter();

    await painter.strokeWith("pen", 4, [
      [40, 40],
      [50, 45],
    ]);
    const afterPen = painter.canvasBytes();

    await painter.strokeWith("touch", 5, [
      [10, 10],
      [30, 30],
    ]);

    expect(anyPixelChanged(afterPen, painter.canvasBytes())).toBe(false);
    painter.cleanup();
  });

  it("takes fingers again in a session with no pen in it", async () => {
    // What a reload looks like to the painter: the latch is gone, and the
    // tablet whose stylus has a flat battery is not locked out of its canvas.
    const painter = await mountPainter();
    const before = painter.canvasBytes();

    await painter.strokeWith("touch", 6, [
      [12, 12],
      [32, 32],
    ]);

    expect(anyPixelChanged(before, painter.canvasBytes())).toBe(true);
    painter.cleanup();
  });
});
