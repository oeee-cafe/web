import { useEffect, useRef, useCallback, useState } from "react";
import { DrawingEngine } from "../DrawingEngine";
import { useCanvasHistory } from "./useCanvasHistory";
import type { BrushType, DrawingState } from "../types/drawing";
import {
  brushTypeFor,
  isImmediateTool,
  isRegionTool,
  isTextTool,
  type RegionTool,
} from "../neo/tools";
import { RegionDrag, type RegionRect } from "../neo/regionDrag";
import { StrokeSmoother, strokeSmootherSizeFor } from "../neo/strokeSmoother";
import type { BezierPreviewStyle } from "../neo/regionPreview";
import { screenToArtwork } from "../neo/canvasTransform";
import { notePointerType, penPreferred } from "../utils/penPreference";

/**
 * Copies the control points so a preview can move a handle without writing it
 * back into the gesture state. Order is NEO's own: start, both handles, end.
 */
function bezierPreviewPoints(points: number[]): number[] {
  return points.slice();
}

/**
 * The shared type, re-exported rather than restated.
 *
 * This was a second declaration of the same shape, and it had already drifted
 * once -- it was missing `drawType`, which is what left the toolbox unable to
 * offer line or bezier. A copy that has to be kept in step by hand will not
 * be, so there is only one now.
 */
export type { DrawingState } from "../types/drawing";

/** The mask a stroke is drawn through, as the engine wants it. */
function applyMask(engine: DrawingEngine, state: DrawingState): void {
  const type = state.maskType ?? 0;
  engine.maskType = type;
  if (!type) return;
  const hex = state.maskColor ?? "#000000";
  engine.maskColor = [
    parseInt(hex.slice(1, 3), 16),
    parseInt(hex.slice(3, 5), 16),
    parseInt(hex.slice(5, 7), 16),
  ];
}

interface DrawingEventCallbacks {
  onPointerDown?: () => void;
  onDrawLine?: (
    fromX: number,
    fromY: number,
    toX: number,
    toY: number,
    brushSize: number,
    brushType: BrushType,
    r: number,
    g: number,
    b: number,
    opacity: number
  ) => void;
  onDrawPoint?: (
    x: number,
    y: number,
    brushSize: number,
    brushType: BrushType,
    r: number,
    g: number,
    b: number,
    opacity: number
  ) => void;
  /** The colour under a right press, for the caller to adopt. */
  onPickColor?: (color: { r: number; g: number; b: number }) => void;
  /** Whether the toolbox's sticky right-click button is armed. */
  isVirtualRight?: () => boolean;
  /** Called once the armed press has been spent, so the button can release. */
  onVirtualRightUsed?: () => void;
  onFill?: (
    x: number,
    y: number,
    r: number,
    g: number,
    b: number,
    opacity: number
  ) => void;
  onPointerUp?: () => void;
  /** The rubber-band rectangle as it is dragged, or null when it ends. */
  onRegionPreview?: (rect: RegionRect | null) => void;
  /** A straight line was drawn from `from` to `to`. */
  onLine?: (
    from: { x: number; y: number },
    to: { x: number; y: number },
    brushSize: number,
    brushType: BrushType,
    color: { r: number; g: number; b: number; a: number },
    layer: "foreground" | "background"
  ) => void;
  /** Endpoints while a line is dragged out, or null when it ends. */
  onLinePreview?: (
    from: { x: number; y: number } | null,
    to: { x: number; y: number } | null
  ) => void;
  /** The text tool was clicked; open an editor at this canvas point. */
  onTextPlace?: (x: number, y: number) => void;
  onBezier?: (
    points: number[],
    brushSize: number,
    brushType: BrushType,
    color: { r: number; g: number; b: number; a: number },
    layerType: "foreground" | "background"
  ) => void;
  onBezierPreview?: (
    points: number[] | null,
    step: number,
    style: BezierPreviewStyle
  ) => void;
  /**
   * The pointer moved over the canvas, or left it (null).
   *
   * Reported whether or not a button is down, because NEO's brush cursor
   * follows the pointer either way -- it is how you see what size you are
   * about to draw at before drawing anything.
   */
  onHoverMove?: (at: { x: number; y: number } | null) => void;
  /** A tool that acts on click was used; record it. */
  onEraseAll?: (layer: "foreground" | "background") => void;
  /** A region tool was released over `rect`; record it. */
  onRegionCommit?: (
    tool: RegionTool,
    layer: "foreground" | "background",
    rect: RegionRect,
    color: { r: number; g: number; b: number; a: number },
    brushSize: number
  ) => void;
}

export const useBaseDrawing = (
  canvasRef: React.RefObject<HTMLCanvasElement | null>,
  appRef: React.RefObject<HTMLDivElement | null>,
  drawingState: DrawingState,
  onHistoryChange?: (canUndo: boolean, canRedo: boolean) => void,
  zoomLevel?: number,
  canvasWidth?: number,
  canvasHeight?: number,
  onDrawingChange?: () => void,
  containerRef?: React.RefObject<HTMLDivElement | null>,
  isDrawingDisabled: boolean = false,
  callbacks?: DrawingEventCallbacks,
  // In remote-sync (collaborative) mode the callbacks own applying strokes to
  // the layers and undo history; this hook only tracks pointer state.
  remoteSync: boolean = false
) => {
  const contextRef = useRef<CanvasRenderingContext2D | null>(null);
  const drawingEngineRef = useRef<DrawingEngine | null>(null);
  /**
   * Forces the render that lets callers see the engine.
   *
   * The engine is held in a ref and handed out by reading that ref, so
   * building it changes nothing anybody re-renders for: a caller asking "is
   * there an engine yet" keeps getting the answer from before it existed,
   * until something unrelated happens to re-render. Readiness was resolving on
   * exactly that accident -- the initial undo-state report -- and stopped the
   * moment there was a reason not to make that report.
   */
  const [, noticeEngine] = useState(0);
  const isInitializedRef = useRef(false);
  const onHistoryChangeRef = useRef(onHistoryChange);
  const isDrawingRef = useRef(false);

  const history = useCanvasHistory(30);

  // Update the ref when callback changes
  const onDrawingChangeRef = useRef(onDrawingChange);
  useEffect(() => {
    onHistoryChangeRef.current = onHistoryChange;
    onDrawingChangeRef.current = onDrawingChange;
  }, [onHistoryChange, onDrawingChange]);

  // Initialize drawing engine
  const initializeDrawing = useCallback(() => {
    if (!canvasRef.current || !canvasWidth || !canvasHeight) return;
    if (isInitializedRef.current && drawingEngineRef.current) return;

    const canvas = canvasRef.current;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    ctx.imageSmoothingEnabled = false;
    canvas.style.imageRendering = "pixelated";
    contextRef.current = ctx;

    // Create and initialize drawing engine
    drawingEngineRef.current = new DrawingEngine(canvasWidth, canvasHeight);
    noticeEngine((seen) => seen + 1);
    drawingEngineRef.current.initialize(ctx);

    // Save initial blank state to history
    if (
      drawingEngineRef.current.layers.foreground &&
      drawingEngineRef.current.layers.background
    ) {
      history.saveState(
        drawingEngineRef.current.layers.foreground,
        drawingEngineRef.current.layers.background,
        drawingEngineRef.current.imageWidth,
        drawingEngineRef.current.imageHeight,
        false,
        false
      );
      onHistoryChangeRef.current?.(history.canUndo(), history.canRedo());
    }

    isInitializedRef.current = true;
  }, [canvasRef, history, canvasWidth, canvasHeight]);

  // Drawing state refs
  const drawingStateRef = useRef({
    isDrawing: false,
    prevX: 0,
    prevY: 0,
    currentX: 0,
    currentY: 0,
    isPanning: false,
    panStartX: 0,
    panStartY: 0,
    panLastX: 0,
    panLastY: 0,
    activePointerId: null as number | null,
  });

  /**
   * How far the pen has to travel before another segment is worth drawing.
   *
   * A distance gate rather than the time gate this used to have beside it. A
   * 12ms throttle drops samples during fast motion, which is when a stroke
   * most needs them -- a quick flick came out as a polygon. Distance drops
   * only the samples that add no shape, so a slow careful line still costs
   * few points and a fast one keeps all the ones that describe it.
   */
  const MIN_MOVE_DISTANCE = 1.5;

  /**
   * How long a finger's press is held before it is allowed to become a stroke.
   *
   * The two fingers of a pinch do not land together -- thirty to eighty
   * milliseconds apart is ordinary -- so a press acted on the instant it
   * arrives leaves a mark at the start of every zoom. Waiting gives the second
   * finger time to arrive and take the press back.
   *
   * Undoing the mark afterwards is not an option in its place: by the time
   * anyone knows the gesture was a pinch it has already been broadcast to the
   * room and written into the replay, and reaching back for it locally would
   * leave this client disagreeing with every other one about the drawing.
   *
   * Touch only. A pen or a mouse cannot be half of a pinch, and drawing with
   * either stays immediate.
   */
  const TOUCH_PRESS_HOLD_MS = 70;

  /** A touch press waiting to see whether a second finger joins it. */
  const pendingTouchPressRef = useRef<{
    pointerId: number;
    begin: () => void;
    moves: PointerEvent[];
    timer: ReturnType<typeof setTimeout>;
  } | null>(null);

  /** True while a pinch owns the fingers and the painter must not read them. */
  const interactionSuspendedRef = useRef(false);

  /**
   * Set by `setupDrawingEvents`, because the state it has to unwind lives in
   * that closure and nothing outside it can reach the pointer handlers.
   */
  const abortGestureRef = useRef<() => void>(() => {});

  /**
   * Averages the pen's tremor out of the stroke in progress
   * (Drawpile's smoother; see `strokeSmoother.ts`). Sized per stroke from the
   * kind of pointer that started it, and drained at pointer-up so the stroke
   * ends where the pen did.
   */
  const smootherRef = useRef(new StrokeSmoother(0));
  /** The last point a segment was actually drawn to, in canvas coordinates. */
  const lastDrawnRef = useRef<{ x: number; y: number } | null>(null);

  const currentDrawingStateRef = useRef(drawingState);
  const isDrawingDisabledRef = useRef(isDrawingDisabled);
  const remoteSyncRef = useRef(remoteSync);
  // Settings captured at pointer down and held for the duration of the stroke,
  // NEO's prepareDrawing. Null between strokes.
  const strokeParamsRef = useRef<DrawingState | null>(null);
  /** Live rectangle drag for the region tools. */
  const regionDragRef = useRef<RegionDrag | null>(null);
  /** Press point while a straight line is being dragged out. */
  // A bezier is built across three separate press/release cycles, so unlike
  // every other gesture its state has to survive pointer-up. NEO's step
  // counter, verbatim: 0 = dragging the chord, 1 = placing the first handle,
  // 2 = placing the second, which commits.
  const bezierRef = useRef<{
    step: number;
    points: number[];
    params: DrawingState | null;
  }>({ step: 0, points: [], params: null });

  const lineStartRef = useRef<{ x: number; y: number } | null>(null);

  const emitBezierPreview = useCallback(
    (points: number[] | null, step: number, params: DrawingState | null) => {
      const state = params ?? currentDrawingStateRef.current;
      callbacks?.onBezierPreview?.(points, step, {
        color: [
          parseInt(state.color.slice(1, 3), 16),
          parseInt(state.color.slice(3, 5), 16),
          parseInt(state.color.slice(5, 7), 16),
          state.opacity,
        ],
        width: state.brushSize,
      });
    },
    [callbacks]
  );

  useEffect(() => {
    currentDrawingStateRef.current = drawingState;
  }, [drawingState]);

  useEffect(() => {
    isDrawingDisabledRef.current = isDrawingDisabled;
  }, [isDrawingDisabled]);

  useEffect(() => {
    remoteSyncRef.current = remoteSync;
  }, [remoteSync]);

  // Convert screen coordinates to canvas coordinates
  const getCanvasCoordinates = useCallback((clientX: number, clientY: number) => {
    const canvas = canvasRef.current;
    if (!canvas) return { x: 0, y: 0 };

    const rect = canvas.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return { x: 0, y: 0 };

    return screenToArtwork(
      { x: clientX, y: clientY },
      rect,
      canvas.width,
      canvas.height,
      currentDrawingStateRef.current.isFlippedHorizontal
    );
  }, [canvasRef]);

  // Perform drawing operation on engine
  const performDrawing = useCallback((
    operation: "point" | "line" | "fill",
    coords: { x: number; y: number; prevX?: number; prevY?: number }
  ) => {
    if (!drawingEngineRef.current) return;

    // A stroke is drawn with the settings it started with, as NEO does
    // (prepareDrawing, called from freeHandDownHandler). Both the replay frame
    // and the collaborative stroke message carry one set of parameters for the
    // whole stroke, so reading live state here would let a mid-stroke change
    // -- the [ / ] and pen hotkeys, or the toolbox -- alter the canvas in a way
    // the recording cannot express. Such a change now applies to the next
    // stroke.
    const active = strokeParamsRef.current ?? currentDrawingStateRef.current;

    const r = parseInt(active.color.slice(1, 3), 16);
    const g = parseInt(active.color.slice(3, 5), 16);
    const b = parseInt(active.color.slice(5, 7), 16);
    const effectiveOpacity = active.opacity;

    applyMask(drawingEngineRef.current, active);

    const targetLayer = drawingEngineRef.current.drawTarget[active.layerType];

    // Callbacks run before the local apply; in remote-sync mode they fully
    // own applying the stroke (via the canvas history), so the direct engine
    // application below is skipped.
    if (operation === "fill") {
      callbacks?.onFill?.(Math.floor(coords.x), Math.floor(coords.y), r, g, b, effectiveOpacity);
      if (!remoteSyncRef.current) {
        drawingEngineRef.current.doFloodFill(
          targetLayer,
          Math.floor(coords.x),
          Math.floor(coords.y),
          r,
          g,
          b,
          effectiveOpacity
        );
      }
    } else if (operation === "point") {
      callbacks?.onDrawPoint?.(
        coords.x,
        coords.y,
        active.brushSize,
        brushTypeFor(active.brushType),
        r,
        g,
        b,
        effectiveOpacity
      );
      if (!remoteSyncRef.current) {
        drawingEngineRef.current.drawLine(
          targetLayer,
          coords.x,
          coords.y,
          coords.x,
          coords.y,
          active.brushSize,
          brushTypeFor(active.brushType),
          r,
          g,
          b,
          effectiveOpacity
        );
      }
    } else if (operation === "line" && coords.prevX !== undefined && coords.prevY !== undefined) {
      callbacks?.onDrawLine?.(
        coords.prevX,
        coords.prevY,
        coords.x,
        coords.y,
        active.brushSize,
        brushTypeFor(active.brushType),
        r,
        g,
        b,
        effectiveOpacity
      );
      if (!remoteSyncRef.current) {
        // Segments run from the NEW point back to the previous one, matching
        // NEO (freeHandMoveHandler, and freeHandFast on replay). Bresenham is
        // direction-sensitive, so drawing prev->new here would leave the live
        // canvas a few pixels off from what the recorded replay renders.
        drawingEngineRef.current.drawLine(
          targetLayer,
          coords.x,
          coords.y,
          coords.prevX,
          coords.prevY,
          active.brushSize,
          brushTypeFor(active.brushType),
          r,
          g,
          b,
          effectiveOpacity
        );
      }
    }

    onDrawingChangeRef.current?.();
  }, [callbacks]);

  // Save current state to history
  const saveToHistory = useCallback(() => {
    if (
      drawingEngineRef.current &&
      drawingEngineRef.current.layers.foreground &&
      drawingEngineRef.current.layers.background
    ) {
      history.saveState(
        drawingEngineRef.current.layers.foreground,
        drawingEngineRef.current.layers.background,
        // From the engine, not from the props: these are the dimensions the
        // buffers actually have, and a snapshot measured against anything else
        // reconstructs a canvas of the wrong shape.
        drawingEngineRef.current.imageWidth,
        drawingEngineRef.current.imageHeight,
        true,
        false
      );
      onHistoryChangeRef.current?.(history.canUndo(), history.canRedo());
    }
  }, [history]);

  // Handle drawing events
  const setupDrawingEvents = useCallback(() => {
    const app = appRef.current;
    if (!app) return;

    const discardPendingTouchPress = () => {
      const pending = pendingTouchPressRef.current;
      if (!pending) return;
      pendingTouchPressRef.current = null;
      clearTimeout(pending.timer);
    };

    /**
     * Holds a touch press for long enough to tell a stroke from a pinch.
     *
     * Samples arriving meanwhile are kept rather than dropped, so when the
     * press does turn into a stroke it starts where the finger landed and
     * follows where it has since gone -- a fast flick keeps its shape instead
     * of beginning at the point it had reached when the wait ran out.
     */
    const holdTouchPress = (pointerId: number, begin: () => void) => {
      discardPendingTouchPress();
      pendingTouchPressRef.current = {
        pointerId,
        begin,
        moves: [],
        timer: setTimeout(() => flushPendingTouchPress(), TOUCH_PRESS_HOLD_MS),
      };
    };

    const flushPendingTouchPress = () => {
      const pending = pendingTouchPressRef.current;
      if (!pending) return;
      pendingTouchPressRef.current = null;
      clearTimeout(pending.timer);
      // The press may have been released or taken over while it waited.
      if (drawingStateRef.current.activePointerId !== pending.pointerId) return;
      pending.begin();
      for (const move of pending.moves) handlePointerMove(move);
    };

    const handlePointerDown = (e: PointerEvent) => {
      const target = e.target as Element;
      const controlsElement = document.getElementById("controls");

      if (controlsElement?.contains(target as Node)) return;

      if (isDrawingDisabledRef.current) return;

      if (
        !(
          target.id === "canvas" ||
          target.closest("#canvas") ||
          (target.closest("#app") && !target.closest("#controls"))
        )
      ) {
        return;
      }

      e.preventDefault();

      notePointerType(e.pointerType);

      if (interactionSuspendedRef.current) return;

      // Once a pen has been used here, fingers navigate rather than draw: they
      // still pinch, and the pinch still pans, but the marks are the pen's.
      // Without this a resting hand draws, because it reaches the glass before
      // the nib does and there is nothing at that moment to tell it apart from
      // someone drawing with a finger. The pan tool is the one exception --
      // moving the canvas is all it can do, so any pointer may.
      if (
        e.pointerType === "touch" &&
        penPreferred() &&
        currentDrawingStateRef.current.brushType !== "pan"
      ) {
        return;
      }

      // A pen landing while a finger's press is still being held has caught the
      // palm that arrived a moment before it. Nothing has been drawn yet -- that
      // is what the hold is for -- so the press can be dropped whole and the pen
      // given the canvas. This is the case the latch above cannot cover: the
      // first pen of the session, which nothing has had a chance to learn from
      // yet.
      if (e.pointerType === "pen" && pendingTouchPressRef.current) {
        cleanupPointerState(pendingTouchPressRef.current.pointerId);
      }

      if (
        drawingStateRef.current.activePointerId !== null &&
        drawingStateRef.current.activePointerId !== e.pointerId
      )
        return;

      drawingStateRef.current.activePointerId = e.pointerId;

      try {
        app.setPointerCapture(e.pointerId);
      } catch (error) {
        console.warn("Failed to capture pointer:", error);
      }

      // NEO's right press is the eyedropper: it takes the colour under the
      // pointer instead of drawing. Ctrl and Alt say the same thing, as they
      // do in NEO, and the toolbox's sticky button says it once for a device
      // with no second button at all.
      const virtualRight = callbacks?.isVirtualRight?.() ?? false;
      if (e.button === 2 || e.ctrlKey || e.altKey || virtualRight) {
        const coords = getCanvasCoordinates(e.clientX, e.clientY);
        const picked = drawingEngineRef.current?.pickVisibleColor(
          coords.x,
          coords.y,
        );
        if (picked) callbacks?.onPickColor?.(picked);
        // One press per press of the button, like NEO's.
        if (virtualRight) callbacks?.onVirtualRightUsed?.();
        drawingStateRef.current.activePointerId = null;
        return;
      }

      if (
        e.button === 1 ||
        (e.pointerType === "touch" && e.buttons === 0) ||
        (e.button === 0 && currentDrawingStateRef.current.brushType === "pan")
      ) {
        drawingStateRef.current.isPanning = true;
        drawingStateRef.current.panStartX = e.clientX;
        drawingStateRef.current.panStartY = e.clientY;
        drawingStateRef.current.panLastX = e.clientX;
        drawingStateRef.current.panLastY = e.clientY;
        return;
      }

      if (
        (e.button === 0 || e.pointerType === "touch" || e.pointerType === "pen") &&
        currentDrawingStateRef.current.brushType !== "pan"
      ) {
        const beginPress = () => {
          const coords = getCanvasCoordinates(e.clientX, e.clientY);
          isDrawingRef.current = true;

          // Freeze the settings this stroke will be drawn and recorded with
          strokeParamsRef.current = { ...currentDrawingStateRef.current };

          // Opens an editor at the click and commits on Enter, so the pointer
          // does nothing else here.
          if (isTextTool(strokeParamsRef.current.brushType)) {
            callbacks?.onTextPlace?.(coords.x, coords.y);
            cleanupPointerState(e.pointerId);
            return;
          }

          // Acts on the press itself, with nothing to drag out
          if (isImmediateTool(strokeParamsRef.current.brushType)) {
            const layer = strokeParamsRef.current.layerType;
            if (!remoteSyncRef.current) {
              drawingEngineRef.current?.eraseAll(layer);
              saveToHistory();
            }
            callbacks?.onEraseAll?.(layer);
            onDrawingChangeRef.current?.();
            cleanupPointerState(e.pointerId);
            return;
          }

          // Bezier spans three gestures. Only the first press seeds the curve
          // and freezes the settings; the two that follow are handled entirely
          // on release, so they must not disturb what is already in flight.
          if (
            strokeParamsRef.current.drawType === "bezier" &&
            !isRegionTool(strokeParamsRef.current.brushType)
          ) {
            const bezier = bezierRef.current;
            if (bezier.step === 0) {
              bezier.points = [coords.x, coords.y];
              bezier.params = strokeParamsRef.current;
              emitBezierPreview(
                [coords.x, coords.y, coords.x, coords.y],
                0,
                bezier.params
              );
            }
            return;
          }

          // A drawing tool in line mode takes two points and commits on
          // release, so it also skips the freehand path.
          if (
            strokeParamsRef.current.drawType === "line" &&
            !isRegionTool(strokeParamsRef.current.brushType)
          ) {
            lineStartRef.current = coords;
            callbacks?.onLinePreview?.(coords, coords);
            return;
          }

          // Region tools drag out a rectangle and act on release, so they take
          // none of the stroke path below.
          if (isRegionTool(strokeParamsRef.current.brushType)) {
            const drag = new RegionDrag(
              drawingEngineRef.current?.imageWidth ?? 0,
              drawingEngineRef.current?.imageHeight ?? 0
            );
            drag.begin(coords);
            regionDragRef.current = drag;
            callbacks?.onRegionPreview?.(drag.current());
            return;
          }

          // Notify callback that drawing started
          callbacks?.onPointerDown?.();

          if (currentDrawingStateRef.current.brushType === "fill") {
            performDrawing("fill", coords);
            if (!remoteSyncRef.current) {
              saveToHistory();
            }
            isDrawingRef.current = false;
          } else {
            // A pen's samples get averaged; a touch digitiser's are smooth
            // already and averaging them twice only adds lag.
            smootherRef.current = new StrokeSmoother(
              strokeSmootherSizeFor(e.pointerType)
            );
            const first = smootherRef.current.push(coords.x, coords.y);
            lastDrawnRef.current = { x: first.x, y: first.y };

            performDrawing("point", first);
            drawingStateRef.current.isDrawing = true;
            drawingStateRef.current.currentX = first.x;
            drawingStateRef.current.currentY = first.y;
            drawingStateRef.current.prevX = drawingStateRef.current.currentX;
            drawingStateRef.current.prevY = drawingStateRef.current.currentY;
          }
        };

        if (e.pointerType === "touch") {
          holdTouchPress(e.pointerId, beginPress);
          return;
        }
        beginPress();
      }
    };

    const cleanupPointerState = (pointerId: number) => {
      if (pendingTouchPressRef.current?.pointerId === pointerId) {
        discardPendingTouchPress();
      }

      if (app.hasPointerCapture(pointerId)) {
        try {
          app.releasePointerCapture(pointerId);
        } catch (error) {
          console.warn("Failed to release pointer capture:", error);
        }
      }

      if (drawingStateRef.current.activePointerId === pointerId) {
        drawingStateRef.current.activePointerId = null;
        drawingStateRef.current.isDrawing = false;
        drawingStateRef.current.isPanning = false;
        isDrawingRef.current = false;
        strokeParamsRef.current = null;
        if (lineStartRef.current) {
          lineStartRef.current = null;
          callbacks?.onLinePreview?.(null, null);
        }
        if (regionDragRef.current) {
          regionDragRef.current.cancel();
          regionDragRef.current = null;
          callbacks?.onRegionPreview?.(null);
        }

        // Canonical Neo clears the joint-dedup state at the end of every
        // stroke (tools.js freeHandUpHandler) and again after each stroke on
        // replay. Carrying it across strokes would suppress the opening dot of
        // a stroke that starts where the last one ended, while the replay --
        // which does reset -- still draws it. Remote-sync mode drives this
        // state externally per user, so leave it alone there.
        if (!remoteSyncRef.current) {
          drawingEngineRef.current?.setStrokeState(null);
        }
      }
    };

    const handlePointerUp = (e: PointerEvent) => {
      // A press let go inside the hold window was a tap, not half a pinch,
      // and a tap draws a dot: start it now so this release has a stroke to
      // finish.
      if (pendingTouchPressRef.current?.pointerId === e.pointerId) {
        flushPendingTouchPress();
      }

      if (drawingStateRef.current.activePointerId !== e.pointerId) return;

      if (isDrawingDisabledRef.current && !drawingStateRef.current.isPanning) return;

      const bezier = bezierRef.current;
      if (bezier.params && bezier.points.length > 0) {
        const at = getCanvasCoordinates(e.clientX, e.clientY);
        const params = bezier.params;
        bezier.step++;

        if (bezier.step === 1) {
          // The drag set both endpoints; the handles start on top of them so
          // the preview reads as a straight chord until one is moved.
          bezier.points = [
            bezier.points[0], bezier.points[1],
            bezier.points[0], bezier.points[1],
            at.x, at.y,
            at.x, at.y,
          ];
          emitBezierPreview(bezierPreviewPoints(bezier.points), 1, bezier.params);
          cleanupPointerState(e.pointerId);
          return;
        }

        if (bezier.step === 2) {
          bezier.points[2] = at.x;
          bezier.points[3] = at.y;
          emitBezierPreview(bezierPreviewPoints(bezier.points), 2, bezier.params);
          cleanupPointerState(e.pointerId);
          return;
        }

        // Third release: the second handle lands and the curve commits.
        bezier.points[4] = at.x;
        bezier.points[5] = at.y;
        const points = bezierPreviewPoints(bezier.points);
        bezierRef.current = { step: 0, points: [], params: null };
        emitBezierPreview(null, 0, params);

        const color = {
          r: parseInt(params.color.slice(1, 3), 16),
          g: parseInt(params.color.slice(3, 5), 16),
          b: parseInt(params.color.slice(5, 7), 16),
          a: params.opacity,
        };
        const brush = brushTypeFor(params.brushType);
        if (!remoteSyncRef.current && drawingEngineRef.current) {
          applyMask(drawingEngineRef.current, params);
          drawingEngineRef.current.drawBezier(
            params.layerType,
            points as [number, number, number, number, number, number, number, number],
            params.brushSize, brush, color
          );
          saveToHistory();
        }
        callbacks?.onBezier?.(points, params.brushSize, brush, color, params.layerType);
        onDrawingChangeRef.current?.();
        cleanupPointerState(e.pointerId);
        return;
      }

      const from = lineStartRef.current;
      if (from) {
        const to = getCanvasCoordinates(e.clientX, e.clientY);
        lineStartRef.current = null;
        callbacks?.onLinePreview?.(null, null);

        const params = strokeParamsRef.current;
        if (params) {
          const color = {
            r: parseInt(params.color.slice(1, 3), 16),
            g: parseInt(params.color.slice(3, 5), 16),
            b: parseInt(params.color.slice(5, 7), 16),
            a: params.opacity,
          };
          const brush = brushTypeFor(params.brushType);
          if (!remoteSyncRef.current && drawingEngineRef.current) {
            applyMask(drawingEngineRef.current, params);
            // Drawn new -> previous, as NEO draws every segment
            drawingEngineRef.current.drawLine(
              drawingEngineRef.current.drawTarget[params.layerType],
              to.x, to.y, from.x, from.y,
              params.brushSize, brush, color.r, color.g, color.b, color.a
            );
            drawingEngineRef.current.setStrokeState(null);
            saveToHistory();
          }
          callbacks?.onLine?.(from, to, params.brushSize, brush, color, params.layerType);
          onDrawingChangeRef.current?.();
        }
        cleanupPointerState(e.pointerId);
        return;
      }

      const drag = regionDragRef.current;
      if (drag?.active) {
        const rect = drag.commit(getCanvasCoordinates(e.clientX, e.clientY));
        regionDragRef.current = null;
        callbacks?.onRegionPreview?.(null);

        const params = strokeParamsRef.current;
        if (rect && params && isRegionTool(params.brushType)) {
          const color = {
            r: parseInt(params.color.slice(1, 3), 16),
            g: parseInt(params.color.slice(3, 5), 16),
            b: parseInt(params.color.slice(5, 7), 16),
            a: params.opacity,
          };
          if (!remoteSyncRef.current && drawingEngineRef.current) {
            applyMask(drawingEngineRef.current, params);
            drawingEngineRef.current.applyRegionTool(
              params.brushType,
              params.layerType,
              rect,
              color,
              params.brushSize
            );
            saveToHistory();
          }
          callbacks?.onRegionCommit?.(
            params.brushType,
            params.layerType,
            rect,
            color,
            params.brushSize
          );
          onDrawingChangeRef.current?.();
        }
        cleanupPointerState(e.pointerId);
        return;
      }

      if (e.button === 1 || drawingStateRef.current.isPanning) {
        drawingStateRef.current.isPanning = false;
      } else {
        if (
          (e.button === 0 || e.pointerType === "touch" || e.pointerType === "pen") &&
          isDrawingRef.current
        ) {
          finishActiveStroke();
        }
        isDrawingRef.current = false;
      }

      cleanupPointerState(e.pointerId);
    };

    const handlePointerCancel = (e: PointerEvent) => {
      cleanupPointerState(e.pointerId);
    };

    const handlePointerLeave = (e: PointerEvent) => {
      const relatedTarget = e.relatedTarget as Element | null;
      const shouldCleanup = !relatedTarget || !app.contains(relatedTarget);

      if (shouldCleanup) {
        callbacks?.onHoverMove?.(null);
        cleanupPointerState(e.pointerId);
      }
    };

    /**
     * Smooths one raw sample and draws the segment it earns, if any.
     *
     * The gate is on the *smoothed* point, not the raw one: what matters is
     * whether the line being drawn has moved, and after averaging a jittery
     * pen held still barely does.
     */
    const drawSmoothedTo = (raw: { x: number; y: number }) => {
      const point = smootherRef.current.push(raw.x, raw.y);
      const last = lastDrawnRef.current;
      if (last) {
        const dx = point.x - last.x;
        const dy = point.y - last.y;
        if (Math.sqrt(dx * dx + dy * dy) < MIN_MOVE_DISTANCE) return;
      }
      lastDrawnRef.current = { x: point.x, y: point.y };

      drawingStateRef.current.prevX = drawingStateRef.current.currentX;
      drawingStateRef.current.prevY = drawingStateRef.current.currentY;
      drawingStateRef.current.currentX = point.x;
      drawingStateRef.current.currentY = point.y;

      performDrawing("line", {
        x: drawingStateRef.current.currentX,
        y: drawingStateRef.current.currentY,
        prevX: drawingStateRef.current.prevX,
        prevY: drawingStateRef.current.prevY,
      });
    };

    /**
     * Walks the smoothed line up to where the pen actually stopped.
     *
     * The average trails the pen by half its window, so without this every
     * stroke would end a few pixels short of where it was released --
     * Drawpile's `smoother_drain`.
     */
    /**
     * Closes the stroke in progress the way a release would.
     *
     * Used by the release itself, and by a pinch taking the fingers away in
     * the middle of one: what has been drawn by then is already on the other
     * clients' canvases and in the replay, so the stroke has to be finished
     * rather than abandoned half-recorded.
     */
    const finishActiveStroke = () => {
      // Before the history entry and before the stroke is closed: these
      // segments belong to the stroke that is ending.
      if (drawingStateRef.current.isDrawing) finishSmoothedStroke();
      if (!remoteSyncRef.current) {
        saveToHistory();
      }
      callbacks?.onPointerUp?.();
      isDrawingRef.current = false;
    };

    const finishSmoothedStroke = () => {
      for (const point of smootherRef.current.drain()) {
        const last = lastDrawnRef.current;
        if (last && last.x === point.x && last.y === point.y) continue;
        lastDrawnRef.current = { x: point.x, y: point.y };
        drawingStateRef.current.prevX = drawingStateRef.current.currentX;
        drawingStateRef.current.prevY = drawingStateRef.current.currentY;
        drawingStateRef.current.currentX = point.x;
        drawingStateRef.current.currentY = point.y;
        performDrawing("line", {
          x: drawingStateRef.current.currentX,
          y: drawingStateRef.current.currentY,
          prevX: drawingStateRef.current.prevX,
          prevY: drawingStateRef.current.prevY,
        });
      }
      lastDrawnRef.current = null;
    };

    const handlePointerMove = (e: PointerEvent) => {
      // Hover counts: a pen that reports it announces itself before it touches
      // down, which sets the latch a whole stroke earlier than first contact.
      notePointerType(e.pointerType);

      if (interactionSuspendedRef.current) return;

      // Before the active-pointer guard: a hovering pointer has no stroke to
      // belong to, and that is exactly when the cursor matters most.
      callbacks?.onHoverMove?.(getCanvasCoordinates(e.clientX, e.clientY));

      const pending = pendingTouchPressRef.current;
      if (pending?.pointerId === e.pointerId) {
        pending.moves.push(e);
        return;
      }

      if (drawingStateRef.current.activePointerId !== e.pointerId) return;

      if (drawingStateRef.current.isPanning) {
        const rawDeltaX = e.clientX - drawingStateRef.current.panLastX;
        const rawDeltaY = e.clientY - drawingStateRef.current.panLastY;

        const currentZoomScale = zoomLevel ? zoomLevel / 100 : 1;
        const deltaX = rawDeltaX / currentZoomScale;
        const deltaY = rawDeltaY / currentZoomScale;

        if (drawingEngineRef.current) {
          const container = containerRef?.current || canvasRef.current || undefined;
          drawingEngineRef.current.updatePanOffset(
            deltaX,
            deltaY,
            container,
            zoomLevel ? zoomLevel / 100 : undefined
          );
        }

        drawingStateRef.current.panLastX = e.clientX;
        drawingStateRef.current.panLastY = e.clientY;
        return;
      }

      const bezierMove = bezierRef.current;
      if (bezierMove.params) {
        const at = getCanvasCoordinates(e.clientX, e.clientY);
        if (bezierMove.step === 0) {
          emitBezierPreview([
            bezierMove.points[0], bezierMove.points[1], at.x, at.y,
          ], 0, bezierMove.params);
        } else {
          // Show the handle that is currently under the pointer without
          // committing it, so the curve tracks the cursor.
          const preview = bezierPreviewPoints(bezierMove.points);
          const slot = bezierMove.step === 1 ? 2 : 4;
          preview[slot] = at.x;
          preview[slot + 1] = at.y;
          emitBezierPreview(preview, bezierMove.step, bezierMove.params);
        }
        return;
      }

      if (lineStartRef.current) {
        callbacks?.onLinePreview?.(
          lineStartRef.current,
          getCanvasCoordinates(e.clientX, e.clientY)
        );
        return;
      }

      const drag = regionDragRef.current;
      if (drag?.active) {
        drag.move(getCanvasCoordinates(e.clientX, e.clientY));
        callbacks?.onRegionPreview?.(drag.current());
        return;
      }

      if (
        !drawingStateRef.current.isDrawing ||
        currentDrawingStateRef.current.brushType === "fill" ||
        currentDrawingStateRef.current.brushType === "pan" ||
        isDrawingDisabledRef.current
      )
        return;

      // Every sample the device actually reported, not just the one the
      // browser chose to wake us with. A pen at a few hundred hertz produces
      // several between frames, and they are the difference between a curve
      // and the chord across it.
      const samples = e.getCoalescedEvents?.() ?? [];
      for (const sample of samples.length > 0 ? samples : [e]) {
        drawSmoothedTo(getCanvasCoordinates(sample.clientX, sample.clientY));
      }
    };

    abortGestureRef.current = () => {
      discardPendingTouchPress();
      if (isDrawingRef.current) finishActiveStroke();
      const active = drawingStateRef.current.activePointerId;
      if (active !== null) cleanupPointerState(active);
      callbacks?.onHoverMove?.(null);
    };

    app.addEventListener("pointerdown", handlePointerDown);
    app.addEventListener("pointerup", handlePointerUp);
    app.addEventListener("pointermove", handlePointerMove);
    app.addEventListener("pointercancel", handlePointerCancel);
    app.addEventListener("pointerleave", handlePointerLeave);

    const preventTouchOnCanvas = (e: TouchEvent) => {
      const target = e.target as Element;
      if (
        target.id === "canvas" ||
        target.closest("#canvas") ||
        (target.closest("#app") && !target.closest("#controls"))
      ) {
        e.preventDefault();
      }
    };

    app.addEventListener("touchstart", preventTouchOnCanvas, { passive: false });
    app.addEventListener("touchmove", preventTouchOnCanvas, { passive: false });
    app.addEventListener("touchend", preventTouchOnCanvas, { passive: false });

    return () => {
      app.removeEventListener("pointerdown", handlePointerDown);
      app.removeEventListener("pointerup", handlePointerUp);
      app.removeEventListener("pointermove", handlePointerMove);
      app.removeEventListener("pointercancel", handlePointerCancel);
      app.removeEventListener("pointerleave", handlePointerLeave);

      app.removeEventListener("touchstart", preventTouchOnCanvas);
      app.removeEventListener("touchmove", preventTouchOnCanvas);
      app.removeEventListener("touchend", preventTouchOnCanvas);
    };
  }, [
    appRef,
    canvasRef,
    containerRef,
    zoomLevel,
    getCanvasCoordinates,
    performDrawing,
    saveToHistory,
    callbacks,
    emitBezierPreview,
  ]);

  useEffect(() => {
    initializeDrawing();
  }, [initializeDrawing]);

  useEffect(() => {
    if (canvasRef.current && !isInitializedRef.current) {
      initializeDrawing();
    }
  }, [canvasRef, initializeDrawing]);

  useEffect(() => {
    const cleanup = setupDrawingEvents();
    return cleanup;
  }, [setupDrawingEvents]);

  // Undo function
  const handleUndo = useCallback(() => {
    const previousState = history.undo();
    if (previousState && contextRef.current && drawingEngineRef.current) {
      history.restoreInto(
        previousState,
        drawingEngineRef.current.layers.foreground,
        drawingEngineRef.current.layers.background
      );

      drawingEngineRef.current.queueLayerUpdate("foreground");
      drawingEngineRef.current.queueLayerUpdate("background");

      onDrawingChange?.();
      onHistoryChangeRef.current?.(history.canUndo(), history.canRedo());
    }
  }, [history, onDrawingChange]);

  // Redo function
  const handleRedo = useCallback(() => {
    const nextState = history.redo();
    if (nextState && contextRef.current && drawingEngineRef.current) {
      history.restoreInto(
        nextState,
        drawingEngineRef.current.layers.foreground,
        drawingEngineRef.current.layers.background
      );

      drawingEngineRef.current.queueLayerUpdate("foreground");
      drawingEngineRef.current.queueLayerUpdate("background");

      onDrawingChange?.();
      onHistoryChangeRef.current?.(history.canUndo(), history.canRedo());
    }
  }, [history, onDrawingChange]);

  // Update canvas zoom
  useEffect(() => {
    if (canvasRef.current && zoomLevel && canvasWidth && canvasHeight) {
      const canvas = canvasRef.current;
      canvas.style.width = `${canvasWidth}px`;
      canvas.style.height = `${canvasHeight}px`;
    }
  }, [zoomLevel, canvasRef, canvasWidth, canvasHeight]);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (drawingEngineRef.current) {
        drawingEngineRef.current.dispose();
        drawingEngineRef.current = null;
      }
    };
  }, []);

  /**
   * Hands the fingers over to something that is not drawing, and takes them
   * back when it is done.
   *
   * Suspending unwinds whatever the pointers had started -- a press still
   * waiting out its hold, a stroke already under way -- so the gesture that
   * took over does not leave one half-open behind it.
   */
  const setInteractionSuspended = useCallback((suspended: boolean) => {
    if (interactionSuspendedRef.current === suspended) return;
    interactionSuspendedRef.current = suspended;
    if (suspended) abortGestureRef.current();
  }, []);

  return {
    context: contextRef.current,
    setInteractionSuspended,
    drawingEngine: drawingEngineRef.current,
    initializeDrawing,
    undo: handleUndo,
    redo: handleRedo,
    canUndo: history.canUndo,
    canRedo: history.canRedo,
    history,
    isDrawingRef,
  };
};
