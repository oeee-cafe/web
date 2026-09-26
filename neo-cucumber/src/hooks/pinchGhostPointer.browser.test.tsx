import { afterEach, describe, expect, it } from "vitest";
import { act, useRef } from "react";
import { createRoot } from "react-dom/client";
import { usePinchZoom } from "./usePinchZoom";
import { PAINTER_REPORT_EVENT, type PainterReport } from "../painterReport";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * A finger whose release never arrives must not haunt the next one.
 *
 * The pinch counts fingers from their press to their release. When a release
 * went missing -- the finger lifted over something outside the painter, or
 * over a node that had left the page -- that finger stayed counted, and every
 * finger after it made a pair with it: one finger dragging zoomed the drawing,
 * and the painter, suspended while two fingers were down, stopped taking the
 * pen as well. Found on a Galaxy Tab, where nothing short of a reload let go.
 */

async function mountPinch() {
  const zooms: number[] = [];
  const suspended = { now: false };
  const reports: PainterReport[] = [];
  const onReport = (e: Event) => reports.push((e as CustomEvent<PainterReport>).detail);
  window.addEventListener(PAINTER_REPORT_EVENT, onReport);

  function Harness() {
    const appRef = useRef<HTMLDivElement>(null);
    const canvasContainerRef = useRef<HTMLDivElement>(null);
    usePinchZoom({
      appRef,
      canvasContainerRef,
      drawingEngine: null,
      currentZoom: 1,
      zoomToScale: (scale) => zooms.push(scale),
      setInteractionSuspended: (s) => { suspended.now = s; },
    });
    return (
      <div id="app" ref={appRef} style={{ width: "400px", height: "400px" }}>
        <div className="canvas-container" ref={canvasContainerRef}
          style={{ width: "200px", height: "200px" }} />
      </div>
    );
  }

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(async () => { root.render(<Harness />); });
  const canvas = container.querySelector(".canvas-container") as HTMLElement;

  const touch = (
    target: EventTarget,
    type: string,
    pointerId: number,
    x: number,
    y: number,
    isPrimary: boolean
  ) => act(() => {
    target.dispatchEvent(new PointerEvent(type, {
      pointerId, pointerType: "touch", isPrimary,
      clientX: x, clientY: y, bubbles: true, cancelable: true,
    }));
  });

  return {
    canvas,
    zooms,
    suspended,
    reports,
    touch,
    unmount: () => act(() => {
      window.removeEventListener(PAINTER_REPORT_EVENT, onReport);
      root.unmount();
      container.remove();
    }),
  };
}

describe("a touch whose release went missing", () => {
  let unmount: (() => void) | undefined;
  afterEach(() => { unmount?.(); unmount = undefined; });

  it("is forgotten when the next hand lands, so one finger does not pinch", async () => {
    const pinch = await mountPinch();
    unmount = pinch.unmount;

    // Down, and never up.
    await pinch.touch(pinch.canvas, "pointerdown", 1, 20, 20, true);

    // A lone finger later: primary, because nothing else is on the glass.
    await pinch.touch(pinch.canvas, "pointerdown", 2, 100, 100, true);
    expect(pinch.suspended.now).toBe(false);
    await pinch.touch(pinch.canvas, "pointermove", 2, 150, 150, true);
    expect(pinch.zooms).toEqual([]);
    await pinch.touch(pinch.canvas, "pointerup", 2, 150, 150, true);
    expect(pinch.suspended.now).toBe(false);

    // And it says so, with the press that never came back in the trail.
    expect(pinch.reports).toHaveLength(1);
    expect(pinch.reports[0].details.forgotten).toEqual([1]);
    expect(pinch.reports[0].details.trail).toContainEqual(
      expect.objectContaining({ type: "pointerdown", pointerId: 1, connected: true })
    );
  });

  it("is heard when the finger lifts outside the painter", async () => {
    const pinch = await mountPinch();
    unmount = pinch.unmount;

    await pinch.touch(pinch.canvas, "pointerdown", 1, 20, 20, true);
    await pinch.touch(pinch.canvas, "pointerdown", 2, 120, 20, false);
    expect(pinch.suspended.now).toBe(true);

    // Both lift over the page around the painter rather than inside it.
    await pinch.touch(document.body, "pointerup", 1, 20, 500, true);
    await pinch.touch(document.body, "pointerup", 2, 120, 500, false);
    expect(pinch.suspended.now).toBe(false);
  });

  it("still pinches with two real fingers", async () => {
    const pinch = await mountPinch();
    unmount = pinch.unmount;

    await pinch.touch(pinch.canvas, "pointerdown", 1, 20, 20, true);
    await pinch.touch(pinch.canvas, "pointerdown", 2, 120, 20, false);
    expect(pinch.suspended.now).toBe(true);
    await pinch.touch(pinch.canvas, "pointermove", 2, 220, 20, false);
    expect(pinch.zooms.at(-1)).toBeCloseTo(2);
    expect(pinch.reports).toEqual([]);
  });
});
