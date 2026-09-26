import { useEffect, useRef } from "react";
import { PinchGesture } from "../neo/pinchGesture";

interface UsePinchZoomOptions {
  /** The painter ground, which is where touches are listened for. */
  appRef: React.RefObject<HTMLDivElement | null>;
  /** The framed canvas, whose transform carries the zoom and the pan. */
  canvasContainerRef: React.RefObject<HTMLDivElement | null>;
  drawingEngine?: {
    updatePanOffset: (
      deltaX: number,
      deltaY: number,
      container?: HTMLCanvasElement | HTMLDivElement,
      zoomScale?: number
    ) => void;
    /** The scale the frame shows now, ahead of any render still to come. */
    readonly zoom: number;
  } | null;
  /** The zoom in force, as a scale rather than a percentage. */
  currentZoom: number;
  zoomToScale: (scale: number, pointerX?: number, pointerY?: number) => void;
  /**
   * Called when a second finger lands, and again when the last one lifts.
   *
   * While a pinch is running the painter must not treat the fingers as a
   * pen: two fingers on a canvas are a way of looking at it, not of drawing
   * on it.
   */
  setInteractionSuspended: (suspended: boolean) => void;
}

/**
 * Pinch to zoom, and drag the pinch to pan.
 *
 * The zoom is anchored to the point between the fingers, so the artwork under
 * them stays under them, and the same midpoint moving carries the canvas with
 * it -- the two are the one gesture, and applying only the first makes the
 * drawing squirm away from the hand holding it.
 *
 * Scale is measured against the separation the fingers had when the pinch was
 * seeded rather than accumulated sample by sample. Accumulating would fold
 * the ladder's rounding into the running total, so a pinch out and back would
 * not return to the zoom it left.
 */
export function usePinchZoom({
  appRef,
  canvasContainerRef,
  drawingEngine,
  currentZoom,
  zoomToScale,
  setInteractionSuspended,
}: UsePinchZoomOptions) {
  // The handlers are registered once and read the moving parts through refs:
  // re-registering them on every zoom step would drop the gesture halfway.
  const gestureRef = useRef(new PinchGesture());
  const baseZoomRef = useRef(1);
  const currentZoomRef = useRef(currentZoom);
  const zoomToScaleRef = useRef(zoomToScale);
  const engineRef = useRef(drawingEngine);
  const suspendRef = useRef(setInteractionSuspended);

  useEffect(() => {
    currentZoomRef.current = currentZoom;
  }, [currentZoom]);
  useEffect(() => {
    zoomToScaleRef.current = zoomToScale;
  }, [zoomToScale]);
  useEffect(() => {
    engineRef.current = drawingEngine;
  }, [drawingEngine]);
  useEffect(() => {
    suspendRef.current = setInteractionSuspended;
  }, [setInteractionSuspended]);

  useEffect(() => {
    const app = appRef.current;
    if (!app) return;
    const gesture = gestureRef.current;
    // The engine's scale rather than the rendered one: touch samples come
    // faster than React commits, and panning by a stale zoom slides the
    // drawing out from under the fingers.
    const zoomNow = () => engineRef.current?.zoom ?? currentZoomRef.current;

    /**
     * Whether a touch belongs to the view rather than to a control.
     *
     * The canvas and the bare ground around it are the view; the floating
     * toolboxes sit inside the same element and are not. Taking their touches
     * would make a two-finger slip on a panel zoom the drawing.
     */
    const overTheCanvas = (target: EventTarget | null) => {
      if (!(target instanceof Element)) return false;
      return target === app || target.closest(".canvas-container") !== null;
    };

    const handlePointerDown = (e: PointerEvent) => {
      if (e.pointerType !== "touch") return;
      if (!overTheCanvas(e.target)) return;

      // The primary touch is the first finger of a hand that was off the
      // glass, so any finger still counted is one whose release never reached
      // us -- lifted over a node that had since left the page, say. Left in,
      // it pairs with every new finger: one finger then pinches, and the
      // painter stays suspended for the pen as well, until a reload.
      if (e.isPrimary && gesture.pointerCount > 0) {
        gesture.clear();
        suspendRef.current(false);
      }

      const change = gesture.down(e.pointerId, { x: e.clientX, y: e.clientY });
      if (gesture.pointerCount < 2) return;

      // Every press once two fingers are down, not just the one that seeded
      // the pinch: a third finger landing would otherwise start a stroke of
      // its own in the middle of the gesture.
      suspendRef.current(true);
      if (change === "seeded") baseZoomRef.current = zoomNow();
    };

    const handlePointerMove = (e: PointerEvent) => {
      if (e.pointerType !== "touch") return;
      const sample = gesture.move(e.pointerId, { x: e.clientX, y: e.clientY });
      if (!sample) return;

      e.preventDefault();

      // Pan first, in the canvas's own units the way the one-finger pan does,
      // then zoom -- the zoom measures the pointer against the container as
      // it stands, and it should stand where this sample put it.
      const zoom = zoomNow();
      if ((sample.panX !== 0 || sample.panY !== 0) && zoom > 0) {
        engineRef.current?.updatePanOffset(
          sample.panX / zoom,
          sample.panY / zoom,
          canvasContainerRef.current ?? undefined,
          zoom
        );
      }

      zoomToScaleRef.current(
        baseZoomRef.current * sample.scale,
        sample.center.x,
        sample.center.y
      );
    };

    const handlePointerUp = (e: PointerEvent) => {
      if (e.pointerType !== "touch") return;
      const change = gesture.up(e.pointerId);
      if (change === "seeded") baseZoomRef.current = zoomNow();
      // Not when the pinch ends but when the hand leaves: a finger still down
      // after its partner lifted is the tail of the gesture, and letting it
      // draw would put a stroke wherever the pinch happened to finish.
      if (gesture.pointerCount === 0) suspendRef.current(false);
    };

    /**
     * Keeps the browser's own pinch out of ours.
     *
     * The canvas frame already declares `touch-action: none`, but the ground
     * around it does not, and a pinch that starts there would scale the page
     * -- and cancel our pointers on the way. Only multi-touch is taken; a
     * single finger is left alone so nothing else on the page stops scrolling.
     */
    const preventBrowserPinch = (e: TouchEvent) => {
      if (e.touches.length >= 2 && overTheCanvas(e.target)) e.preventDefault();
    };

    // Releases are heard on the window: a finger that went down on the
    // canvas may lift over the site's own header or a panel outside the
    // ground, and a release missed there would leave it counted.
    app.addEventListener("pointerdown", handlePointerDown);
    app.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp, true);
    window.addEventListener("pointercancel", handlePointerUp, true);
    app.addEventListener("touchstart", preventBrowserPinch, { passive: false });
    app.addEventListener("touchmove", preventBrowserPinch, { passive: false });

    return () => {
      app.removeEventListener("pointerdown", handlePointerDown);
      app.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp, true);
      window.removeEventListener("pointercancel", handlePointerUp, true);
      app.removeEventListener("touchstart", preventBrowserPinch);
      app.removeEventListener("touchmove", preventBrowserPinch);
      // A gesture that outlived its listeners would hold the painter
      // suspended, and nothing left would ever let it go.
      if (gesture.pointerCount > 0) suspendRef.current(false);
      gesture.clear();
    };
  }, [appRef, canvasContainerRef]);
}
