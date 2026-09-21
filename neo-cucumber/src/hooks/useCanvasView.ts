import React, { useCallback, useEffect, useRef } from "react";
import type { DrawingEngine } from "../DrawingEngine";
import { drawBrushCursor } from "../neo/brushCursor";
import type { DrawingState } from "../types/drawing";

interface CanvasViewOptions {
  drawingEngine: DrawingEngine | null | undefined;
  drawingState: DrawingState;
  canvasContainerRef: React.RefObject<HTMLDivElement | null>;
  currentZoom: number;
  canvasWidth?: number;
  canvasHeight?: number;
}

/**
 * Everything about *looking at* the canvas, as opposed to drawing on it: the
 * brush cursor, the horizontal flip, and the pan that follows a zoom.
 *
 * These were written out separately in the offline and collaborative apps,
 * and the copies had already diverged -- the offline one never synced the flip
 * to the engine, so its canvas stayed put while `useBaseDrawing` mirrored every
 * pointer coordinate, and strokes landed on the opposite side from the cursor.
 * The bug was only possible because there were two copies to keep in step.
 */
export function useCanvasView({
  drawingEngine,
  drawingState,
  canvasContainerRef,
  currentZoom,
  canvasWidth,
  canvasHeight,
}: CanvasViewOptions) {
  /**
   * NEO's brush cursor, on its own overlay above the preview one. Separate
   * because the two are cleared independently: a rubber band and a brush
   * circle would otherwise wipe each other out on alternate frames.
   */
  const cursorCanvasRef = useRef<HTMLCanvasElement>(null);
  const hoverRef = useRef<{ x: number; y: number } | null>(null);
  // The engine is created after this hook runs, and the cursor only ever
  // paints from an event, so it reaches it through a ref rather than a closure.
  const engineRef = useRef<DrawingEngine | null>(null);

  useEffect(() => {
    engineRef.current = drawingEngine ?? null;
  }, [drawingEngine]);

  const { brushSize, brushType, bgVisible, fgVisible } = drawingState;

  const paintCursor = useCallback(
    (at: { x: number; y: number } | null) => {
      hoverRef.current = at;
      const ctx = cursorCanvasRef.current?.getContext("2d");
      if (!ctx) return;

      // The XOR needs whatever is showing underneath, which is the visible
      // layers over the white the canvas element sits on.
      const engine = engineRef.current;
      const layers = engine
        ? [
            bgVisible ? engine.layers.background : null,
            fgVisible ? engine.layers.foreground : null,
          ].filter((l): l is Uint8ClampedArray => l !== null)
        : [];

      drawBrushCursor(
        ctx,
        at,
        brushSize,
        brushType,
        engine && canvasWidth && canvasHeight
          ? { width: canvasWidth, height: canvasHeight, scale: currentZoom, layers }
          : null
      );
    },
    [brushSize, brushType, bgVisible, fgVisible, canvasWidth, canvasHeight, currentZoom]
  );

  // Redraw where it already is when the brush changes under it, so the circle
  // resizes as the size slider moves rather than at the next mouse move.
  useEffect(() => {
    paintCursor(hoverRef.current);
  }, [paintCursor]);

  /*
   * Mirror the canvas when the flip is on.
   *
   * `useBaseDrawing` mirrors pointer coordinates for it either way, so
   * skipping this does not disable the flip -- it half-applies it.
   */
  useEffect(() => {
    if (!drawingEngine) return;
    drawingEngine.setFlippedHorizontal(
      drawingState.isFlippedHorizontal,
      canvasContainerRef.current || undefined,
      currentZoom
    );
  }, [
    drawingState.isFlippedHorizontal,
    drawingEngine,
    currentZoom,
    canvasContainerRef,
  ]);

  /*
   * Put the canvas back on whole pixels when the window changes size.
   *
   * The frame is flex-centred, so resizing the viewport by an odd number of
   * pixels moves it by half of one, and nothing else re-applies the transform
   * until the next pan or zoom -- leaving the wobble a whole-number zoom is
   * meant to rule out. A zero pan re-runs the clamp and the snap. The visual
   * viewport is watched too, because a phone's URL bar and keyboard change it
   * without resizing the window.
   */
  useEffect(() => {
    if (!drawingEngine) return;
    const resettle = () =>
      drawingEngine.updatePanOffset(
        0,
        0,
        canvasContainerRef.current || undefined,
        currentZoom
      );
    window.addEventListener("resize", resettle);
    window.visualViewport?.addEventListener("resize", resettle);
    return () => {
      window.removeEventListener("resize", resettle);
      window.visualViewport?.removeEventListener("resize", resettle);
    };
  }, [drawingEngine, currentZoom, canvasContainerRef]);

  return { cursorCanvasRef, paintCursor };
}

/**
 * A stable callback that forwards to whatever it is pointed at later.
 *
 * The drawing hook has to be given a hover handler, but the handler needs the
 * engine that same hook creates. Rather than have each app hand-roll a ref and
 * a forwarder around that ordering, this names the pattern once.
 */
export function useDeferredHandler<A>() {
  const target = useRef<(value: A) => void>(() => {});
  const forward = useCallback((value: A) => target.current(value), []);
  const point = useCallback((fn: (value: A) => void) => {
    target.current = fn;
  }, []);
  return [forward, point] as const;
}
