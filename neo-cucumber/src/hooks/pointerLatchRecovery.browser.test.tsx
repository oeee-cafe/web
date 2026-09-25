import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act, useEffect, useRef } from "react";
import { createRoot } from "react-dom/client";
import { usePainterDrawing } from "./usePainterDrawing";
import type { DrawingState } from "../types/drawing";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * A handler that throws must not keep the pointer.
 *
 * The press latches the pointer id and the release clears it, and every
 * callback in between -- previews, history, a session's hooks -- runs on the
 * same stack. When one of them threw, the latch stayed set, and since each
 * contact of a pen is a new pointer id, every press after that was refused
 * as a second pointer. The painter was dead until a reload while other
 * people's strokes kept arriving. The throw that found it was the bezier
 * preview at a fit-to-screen zoom; here the preview is made to throw on
 * purpose, and what is checked is that the next contact still draws.
 *
 * Each contact here carries a fresh pointer id, the way a stylus's do. A
 * mouse keeps one id for its whole life, which is why this never showed on
 * a desktop: its next press matched the stale latch and went through.
 */

const W = 60;
const H = 40;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

type Api = ReturnType<typeof usePainterDrawing>;

async function mountBezierPainter(onBezierPreview: (points: number[] | null) => void) {
  const captured: { api: Api | null } = { api: null };
  const state: DrawingState = {
    brushSize: 4, opacity: 255, color: "#1e2864",
    brushType: "solid", drawType: "bezier",
    layerType: "background", zoomLevel: 100,
    fgVisible: true, bgVisible: true, isFlippedHorizontal: false,
  };

  function Harness() {
    const appRef = useRef<HTMLDivElement>(null);
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const api = usePainterDrawing({
      canvasRef, appRef, drawingState: state,
      zoomLevel: 100, canvasWidth: W, canvasHeight: H,
      previews: { onBezierPreview: (points) => onBezierPreview(points) },
      mode: { kind: "offline" },
    });
    useEffect(() => { captured.api = api; });
    return (
      <div id="app" ref={appRef}>
        <canvas id="canvas" ref={canvasRef} width={W} height={H}
          style={{ width: `${W}px`, height: `${H}px`, display: "block" }} />
      </div>
    );
  }

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(async () => { root.render(<Harness />); });
  for (let i = 0; i < 50 && !captured.api?.drawingEngine; i++) {
    await act(async () => { await sleep(10); root.render(<Harness />); });
  }
  if (!captured.api?.drawingEngine) throw new Error("engine never initialised");

  const canvas = container.querySelector("#canvas") as HTMLCanvasElement;
  const rect = canvas.getBoundingClientRect();
  let contact = 100;
  /** One press-and-release of a pen, on a pointer id nothing has seen before. */
  const tap = async (x: number, y: number) => {
    const pointerId = contact++;
    for (const type of ["pointerdown", "pointerup"]) {
      await act(async () => {
        canvas.dispatchEvent(new PointerEvent(type, {
          pointerId, pointerType: "pen", button: 0,
          buttons: type === "pointerup" ? 0 : 1,
          clientX: rect.left + x, clientY: rect.top + y,
          bubbles: true, cancelable: true,
        }));
      });
    }
  };

  return {
    api: captured.api,
    tap,
    unmount: () => act(() => { root.unmount(); container.remove(); }),
  };
}

describe("a pointer handler that throws", () => {
  const surfaced: unknown[] = [];
  // Registering a listener of our own is what tells the runner these
  // errors are expected here rather than a failure of the test.
  const onError = (event: ErrorEvent) => {
    surfaced.push(event.error);
    event.preventDefault();
  };
  beforeEach(() => {
    surfaced.length = 0;
    window.addEventListener("error", onError);
  });
  afterEach(() => window.removeEventListener("error", onError));

  it("lets go of the pointer, so the next contact is not refused", async () => {
    let thrown = 0;
    const painter = await mountBezierPainter((points) => {
      // The chord (four points) previews fine; a curve with handles is where
      // the pen kernel used to be asked for a width it had no round for.
      if (points && points.length === 8) {
        thrown++;
        throw new TypeError("Cannot read properties of undefined (reading '0')");
      }
    });
    const layer = painter.api.drawingEngine!.layers.background;
    const at = (x: number, y: number) => layer[(y * W + x) * 4 + 3];

    // The chord, as a tap: both ends land at once, and the release moves on
    // to the first handle, whose preview is the one that throws.
    await painter.tap(8, 30);
    expect(thrown).toBe(1);
    expect(surfaced).toHaveLength(1);

    // The first handle is placed; the second's preview throws too.
    await painter.tap(16, 6);
    expect(thrown).toBe(2);

    // The third release commits the curve.
    await painter.tap(42, 6);

    // Had either press been refused, nothing would ever have committed.
    expect(at(8, 30)).toBeGreaterThan(0);
    expect(painter.api.isDrawingRef.current).toBe(false);

    await painter.unmount();
  });
});
