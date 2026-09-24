import { useRef, useCallback } from "react";
import { useBaseDrawing, type DrawingState } from "./useBaseDrawing";
import { ActionRecorder } from "../utils/ActionRecorder";
import { deflateCoverage } from "../utils/rasterCodec";
import type { Mask } from "../neo/mask";
import type { BrushType } from "../types/drawing";
import type { PainterBrush, PainterOperation } from "../operations";
import {
  fillToolTypeFor,
  frameShapeFor,
  type RegionTool,
  type ToolId,
} from "../neo/tools";
import type { RegionRect } from "../neo/regionDrag";
import type { BezierPreviewStyle, PasteDisplay } from "../neo/regionPreview";

// Constants matching Neo's LINETYPE values
const LINETYPE_PEN = 1;
const LINETYPE_ERASER = 2;
const LINETYPE_BRUSH = 3;
const LINETYPE_TONE = 4;
const LINETYPE_DODGE = 5;
const LINETYPE_BURN = 6;
const LINETYPE_BLUR = 7;

/** The line type NEO serialises for each stroked brush. */
export const lineTypeForBrush = (brushType: BrushType): number => {
  switch (brushType) {
    case "eraser":
      return LINETYPE_ERASER;
    case "brush":
      return LINETYPE_BRUSH;
    case "halftone":
      return LINETYPE_TONE;
    case "dodge":
      return LINETYPE_DODGE;
    case "burn":
      return LINETYPE_BURN;
    case "blur":
      return LINETYPE_BLUR;
    default:
      // solid is the remaining drawing brush; fill and pan do not serialize
      // as strokes, but keeping their historical fallback is harmless.
      return LINETYPE_PEN;
  }
};

/**
 * How many points a broadcast stroke may gather before it is sent.
 *
 * A ceiling rather than a target: the interval below usually reaches first.
 * It exists so a very fast pointer cannot build an unbounded message.
 */
const STROKE_CHUNK_POINTS = 32;

/**
 * How long a broadcast stroke may gather for.
 *
 * This is the delay before the rest of the room sees ink that is already on
 * the author's own canvas. Short enough to read as live, long enough to turn
 * a stroke's worth of samples into a handful of messages instead of one per
 * sample.
 */
const STROKE_CHUNK_MS = 50;

export const useOfflineDrawing = (
  canvasRef: React.RefObject<HTMLCanvasElement | null>,
  appRef: React.RefObject<HTMLDivElement | null>,
  drawingState: DrawingState,
  onHistoryChange?: (canUndo: boolean, canRedo: boolean) => void,
  zoomLevel?: number,
  canvasWidth?: number,
  canvasHeight?: number,
  onDrawingChange?: () => void,
  containerRef?: React.RefObject<HTMLDivElement | null>,
  /** Called with the rubber-band rectangle while a region tool is dragged. */
  onRegionPreview?: (rect: RegionRect | null) => void,
  /** Called with the endpoints while a straight line is dragged out. */
  onLinePreview?: (
    from: { x: number; y: number } | null,
    to: { x: number; y: number } | null
  ) => void,
  /** Called when the text tool is clicked, to open an editor there. */
  onTextPlace?: (x: number, y: number) => void,
  /** Called with the curve so far while a bezier is being built. */
  onBezierPreview?: (
    points: number[] | null,
    step: number,
    style: BezierPreviewStyle
  ) => void,
  /** Called as the pointer moves over the canvas, or leaves it. */
  onHoverMove?: (at: { x: number; y: number } | null) => void,
  isDrawingDisabled: boolean = false,
  onOperation?: (operation: PainterOperation) => void,
  onPointerRelease?: () => void,
  /** The colour a right press found, for the host to adopt as the current one. */
  onPickColor?: (color: { r: number; g: number; b: number }) => void,
  /** Whether the toolbox's sticky right-click button is armed. */
  isVirtualRight?: () => boolean,
  /** Called when an armed press has been spent, so the button can release. */
  onVirtualRightUsed?: () => void,
  /** Whether to keep a `.pch` replay; a collaborative host does not. */
  recordReplay: boolean = true,
  /**
   * Copy and paste as NEO does them: the painter switches itself from copy
   * to paste and back, and shows the copy while it is being placed.
   */
  placement?: {
    onToolChange?: (tool: ToolId) => void;
    onPastePreview?: (display: PasteDisplay | null) => void;
  },
) => {
  // Initialize replay recording
  const actionRecorderRef = useRef<ActionRecorder>(
    new ActionRecorder(recordReplay),
  );
  const isFirstPointRef = useRef<boolean>(false);
  const hasCreatedStepRef = useRef<boolean>(false);
  const strokeBoundaryEmittedRef = useRef(false);
  /**
   * Segments of the stroke in progress that have been drawn locally but not
   * yet broadcast, and when the chunk they belong to was opened.
   */
  const strokeChunkRef = useRef<
    | (Extract<PainterOperation, { kind: "stroke" }> & {
        openedAt: number;
        /**
         * True when `points[0]` has already been broadcast and is only here so
         * the next message joins onto it. Without this a dot -- one point,
         * never sent -- looks the same as a chunk holding nothing new.
         */
        carried: boolean;
      })
    | null
  >(null);

  /**
   * The engine, reachable from the callbacks that are handed to the hook that
   * builds it. Declared before them and filled after, because the callbacks
   * only ever run later.
   */
  const engineRef = useRef<import("../DrawingEngine").DrawingEngine | null>(null);

  const emitOperation = useCallback((operation: PainterOperation) => {
    if (!onOperation) return;
    if (operation.kind !== "undo" && operation.kind !== "undo-boundary") {
      onOperation({ kind: "undo-boundary" });
    }
    onOperation(operation);
  }, [onOperation]);

  /**
   * Sends the segments accumulated so far as one stroke, and leaves the chunk
   * open at the point they ended on.
   *
   * The next chunk starts from that same point so the line joins across the
   * boundary, which is how consecutive stroke messages have always met.
   */
  const flushStrokeChunk = useCallback(() => {
    const chunk = strokeChunkRef.current;
    if (!onOperation || !chunk) return;
    if (chunk.points.length <= (chunk.carried ? 1 : 0)) return;
    if (!strokeBoundaryEmittedRef.current) {
      onOperation({ kind: "undo-boundary" });
      strokeBoundaryEmittedRef.current = true;
    }
    onOperation({
      kind: "stroke",
      layer: chunk.layer,
      brushSize: chunk.brushSize,
      brush: chunk.brush,
      color: chunk.color,
      mask: chunk.mask,
      points: chunk.points.slice(),
      ...(chunk.targetActorId === undefined
        ? {}
        : { targetActorId: chunk.targetActorId }),
    });
    const last = chunk.points[chunk.points.length - 1];
    chunk.points = [last];
    chunk.carried = true;
    chunk.openedAt = performance.now();
  }, [onOperation]);

  /**
   * Adds one drawn segment to the stroke being broadcast, flushing when the
   * chunk is big enough or old enough.
   *
   * Every segment used to go out on its own, so a six-second stroke became
   * some hundreds of canonical messages -- hundreds of sequence numbers,
   * history entries and fork entries for one gesture, and the cost of that
   * lands on the whole room's catch-up and on how often the server has to ask
   * for a checkpoint. Drawpile packs thousands of dabs into a message for
   * exactly this reason.
   *
   * The local canvas is painted by the interactive path as the pointer moves,
   * so nothing here delays what the person drawing sees; the wait is only
   * before other people see it, which is what the flush interval bounds.
   */
  const emitStrokeOperation = useCallback((
    operation: Extract<PainterOperation, { kind: "stroke" }>,
  ) => {
    if (!onOperation) return;
    const chunk = strokeChunkRef.current;
    if (!chunk) {
      strokeChunkRef.current = {
        ...operation,
        points: operation.points.slice(),
        openedAt: performance.now(),
        carried: false,
      };
    } else {
      // The segment starts where the last one ended, so only its far end is
      // new to the chunk.
      chunk.points.push(operation.points[operation.points.length - 1]);
    }
    const open = strokeChunkRef.current;
    if (
      open !== null &&
      (open.points.length >= STROKE_CHUNK_POINTS ||
        performance.now() - open.openedAt >= STROKE_CHUNK_MS)
    ) {
      flushStrokeChunk();
    }
  }, [onOperation, flushStrokeChunk]);

  // Callbacks for recording drawing operations
  const callbacks = {
    onPointerDown: useCallback(() => {
      // Mark that this is the start of a new stroke
      // The actual step() call will happen in onDrawLine/onDrawPoint when data is recorded
      isFirstPointRef.current = true;
      hasCreatedStepRef.current = false;
      strokeChunkRef.current = null;
      strokeBoundaryEmittedRef.current = false;
    }, []),

    onDrawLine: useCallback(
      (
        fromX: number,
        fromY: number,
        toX: number,
        toY: number,
        brushSize: number,
        brushType: BrushType,
        r: number,
        g: number,
        b: number,
        opacity: number,
        layer: "foreground" | "background",
        mask: Mask
      ) => {
        // Opacity is already in [0, 255] range - clamp and ensure no NaN
        const alpha = Math.max(0, Math.min(255, Math.floor(opacity || 0)));
        const lineType = lineTypeForBrush(brushType);

        // Ensure color values are valid
        const safeR = Math.max(0, Math.min(255, Math.floor(r || 0)));
        const safeG = Math.max(0, Math.min(255, Math.floor(g || 0)));
        const safeB = Math.max(0, Math.min(255, Math.floor(b || 0)));

        // Ensure coordinates are valid numbers (not NaN or Infinity)
        if (!Number.isFinite(fromX) || !Number.isFinite(fromY) ||
            !Number.isFinite(toX) || !Number.isFinite(toY)) {
          console.warn("Invalid coordinates in onDrawLine:", { fromX, fromY, toX, toY });
          return;
        }

        const to = { x: Math.round(toX), y: Math.round(toY) };
        emitStrokeOperation({
          kind: "stroke",
          layer,
          brushSize,
          brush: brushType as PainterBrush,
          color: { r: safeR, g: safeG, b: safeB, a: alpha },
          points: [{ x: Math.round(fromX), y: Math.round(fromY) }, to],
          mask,
        });

        // Only create action frame and push header once per stroke
        if (!hasCreatedStepRef.current) {
          // First point of stroke - create new action frame and record full header
          actionRecorderRef.current.step();
          hasCreatedStepRef.current = true;

          // A stroke normally opens with onDrawPoint, which writes the header
          // with the press point duplicated. Reaching here means the header is
          // being opened by a segment instead, which NEO's freeHandMove
          // records as (previous, new) so the replay draws that first segment.
          actionRecorderRef.current.push(
            "freeHand",
            layer === "foreground" ? 1 : 0,
            safeR,
            safeG,
            safeB,
            alpha,
            mask.r,
            mask.g,
            mask.b,
            brushSize,
            mask.type,
            lineType,
            Math.round(fromX),
            Math.round(fromY),
            Math.round(toX),
            Math.round(toY)
          );
        } else {
          // Subsequent points - just record coordinates
          actionRecorderRef.current.push(Math.round(toX), Math.round(toY));
        }
      },
      [emitStrokeOperation]
    ),

    onDrawPoint: useCallback(
      (
        x: number,
        y: number,
        brushSize: number,
        brushType: BrushType,
        r: number,
        g: number,
        b: number,
        opacity: number,
        layer: "foreground" | "background",
        mask: Mask
      ) => {
        // Opacity is already in [0, 255] range - clamp and ensure no NaN
        const alpha = Math.max(0, Math.min(255, Math.floor(opacity || 0)));
        const lineType = lineTypeForBrush(brushType);

        // Ensure color values are valid
        const safeR = Math.max(0, Math.min(255, Math.floor(r || 0)));
        const safeG = Math.max(0, Math.min(255, Math.floor(g || 0)));
        const safeB = Math.max(0, Math.min(255, Math.floor(b || 0)));

        // Ensure coordinates are valid numbers (not NaN or Infinity)
        if (!Number.isFinite(x) || !Number.isFinite(y)) {
          console.warn("Invalid coordinates in onDrawPoint:", { x, y });
          return;
        }

        emitStrokeOperation({
          kind: "stroke",
          layer,
          brushSize,
          brush: brushType as PainterBrush,
          color: { r: safeR, g: safeG, b: safeB, a: alpha },
          points: [{ x: Math.round(x), y: Math.round(y) }],
          mask,
        });

        // Single point stroke - create new action frame
        if (!hasCreatedStepRef.current) {
          actionRecorderRef.current.step();
          hasCreatedStepRef.current = true;
        }
        actionRecorderRef.current.push(
          "freeHand",
          layer === "foreground" ? 1 : 0,
          safeR,
          safeG,
          safeB,
          alpha,
          mask.r,
          mask.g,
          mask.b,
          brushSize,
          mask.type,
          lineType,
          Math.round(x),
          Math.round(y),
          Math.round(x),
          Math.round(y)
        );
      },
      [emitStrokeOperation]
    ),

    // The eyedropper reports a colour rather than drawing one, so it neither
    // records nor broadcasts: what it changes is which colour this person is
    // holding.
    onPickColor: onPickColor,
    isVirtualRight: isVirtualRight,
    onVirtualRightUsed: onVirtualRightUsed,

    onFill: useCallback(
      (
        x: number,
        y: number,
        r: number,
        g: number,
        b: number,
        opacity: number,
        layerName: "foreground" | "background",
        mask: Mask
      ) => {
        const layer = layerName === "foreground" ? 1 : 0;
        // Opacity is already in [0, 255] range - clamp and ensure no NaN
        const alpha = Math.max(0, Math.min(255, Math.floor(opacity || 0)));

        // Ensure color values are valid
        const safeR = Math.max(0, Math.min(255, Math.floor(r || 0)));
        const safeG = Math.max(0, Math.min(255, Math.floor(g || 0)));
        const safeB = Math.max(0, Math.min(255, Math.floor(b || 0)));

        // Ensure coordinates are valid numbers (not NaN or Infinity)
        if (!Number.isFinite(x) || !Number.isFinite(y)) {
          console.warn("Invalid coordinates in onFill:", { x, y });
          return;
        }

        // ABGR format: (alpha << 24) | (blue << 16) | (green << 8) | red
        const color = (alpha << 24) | (safeB << 16) | (safeG << 8) | safeR;

        if (!hasCreatedStepRef.current) {
          actionRecorderRef.current.step();
          hasCreatedStepRef.current = true;
        }
        // The replay file keeps the flood as a flood: it is one participant's
        // own drawing, replayed against the canvas it was made on, so the seed
        // reproduces it exactly and costs four numbers instead of a raster.
        actionRecorderRef.current.push("floodFill", layer, Math.round(x), Math.round(y), color);

        const engine = engineRef.current;
        if (!onOperation || !engine) return false;

        // Run it here and send what it covered. A seed replayed elsewhere
        // floods whatever that layer holds at the time, which after an undo
        // underneath it is not the same shape at all.
        const target = engine.drawTarget[layerName];
        const region = engine.floodFillCapturingRegion(
          target, Math.round(x), Math.round(y), safeR, safeG, safeB, alpha,
        );
        // The layer is flooded either way; what is sent is what it covered.
        if (!region) return true;

        const { x: rx, y: ry, width, height, coverage } = region;
        // In the same turn as the flood, so the operation is in the fork
        // before anything else can be emitted or a checkpoint asked for.
        emitOperation({
          kind: "fill-region",
          layer: layerName,
          at: { x: rx, y: ry },
          width,
          height,
          color: { r: safeR, g: safeG, b: safeB, a: alpha },
          coverage: deflateCoverage(coverage),
          mask,
        });
        return true;
      },
      [emitOperation, onOperation]
    ),

    onRegionPreview,
    onToolChange: placement?.onToolChange,
    onPastePreview: placement?.onPastePreview,

    /*
     * A dropped copy, recorded the way NEO records one: the rectangle it
     * was copied from and the offset it was dragged by, so a `.pch` from
     * here reads exactly like one from NEO. The wire carries the rectangle
     * it landed on instead, which a peer's engine pastes at directly --
     * the same pixels, with the offset already applied.
     */
    onPaste: useCallback(
      (
        layer: "foreground" | "background",
        source: RegionRect,
        dx: number,
        dy: number,
        mask: Mask
      ) => {
        const shape = frameShapeFor("paste");
        if (!shape) return;
        const color = {
          r: parseInt(drawingState.color.slice(1, 3), 16),
          g: parseInt(drawingState.color.slice(3, 5), 16),
          b: parseInt(drawingState.color.slice(5, 7), 16),
          a: drawingState.opacity,
        };
        actionRecorderRef.current.pushRegion(
          shape.verb,
          shape.carriesDrawingState,
          layer === "foreground" ? 1 : 0,
          source,
          color,
          drawingState.brushSize,
          [dx, dy],
          mask
        );
        emitOperation({
          kind: "region",
          layer,
          tool: "paste",
          rect: {
            x: source.x + dx,
            y: source.y + dy,
            width: source.width,
            height: source.height,
          },
          color,
          brushSize: drawingState.brushSize,
          mask,
        });
      },
      [drawingState.color, drawingState.opacity, drawingState.brushSize, emitOperation]
    ),

    onLinePreview,
    onTextPlace,
    onBezierPreview,
    onHoverMove,

    onBezier: useCallback(
      (
        points: number[],
        brushSize: number,
        brushType: BrushType,
        color: { r: number; g: number; b: number; a: number },
        layer: "foreground" | "background",
        mask: Mask
      ) => {
        actionRecorderRef.current.pushBezier(
          layer === "foreground" ? 1 : 0,
          lineTypeForBrush(brushType),
          points,
          color,
          brushSize,
          mask
        );
        emitOperation({
          kind: "bezier", layer, brushSize,
          brush: brushType as PainterBrush, color,
          points: points as [number, number, number, number, number, number, number, number],
          mask,
        });
      },
      [emitOperation]
    ),

    onLine: useCallback(
      (
        from: { x: number; y: number },
        to: { x: number; y: number },
        brushSize: number,
        brushType: BrushType,
        color: { r: number; g: number; b: number; a: number },
        layer: "foreground" | "background",
        mask: Mask
      ) => {
        actionRecorderRef.current.pushLine(
          layer === "foreground" ? 1 : 0,
          lineTypeForBrush(brushType),
          from,
          to,
          color,
          brushSize,
          mask
        );
        emitOperation({
          kind: "line", layer, brushSize,
          brush: brushType as PainterBrush, color, from, to,
          mask,
        });
      },
      [emitOperation]
    ),

    /** NEO records a cleared layer as ["eraseAll", layer]. */
    onEraseAll: useCallback((layer: "foreground" | "background") => {
      actionRecorderRef.current.step();
      actionRecorderRef.current.push(
        "eraseAll",
        layer === "foreground" ? 1 : 0
      );
      emitOperation({ kind: "clear-layer", layer });
    }, [emitOperation]),

    /**
     * A region tool was applied; record it as its own frame. The verb and
     * whether it carries the drawing state come from the tool table, since
     * getting that boundary wrong shifts every field after it.
     */
    onRegionCommit: useCallback(
      (
        tool: RegionTool,
        layer: "foreground" | "background",
        rect: RegionRect,
        color: { r: number; g: number; b: number; a: number },
        brushSize: number,
        mask: Mask
      ) => {
        const shape = frameShapeFor(tool);
        if (!shape) return;
        const fillType = fillToolTypeFor(tool);
        actionRecorderRef.current.pushRegion(
          shape.verb,
          shape.carriesDrawingState,
          layer === "foreground" ? 1 : 0,
          rect,
          color,
          brushSize,
          // A `fill` frame ends with which shape it is: NEO's fill action
          // reads it from item[15] and hands it to doFill, whose getMaskFunc
          // draws nothing for a type it does not know. This was left off,
          // so every rectangle and ellipse drawn here replayed as blank --
          // in NEO and in our own viewer alike -- while the canvas it was
          // recorded on showed it.
          fillType !== null ? [fillType] : [],
          mask
        );
        emitOperation({
          kind: "region", layer, tool, rect, color, brushSize,
          mask,
        });
      },
      [emitOperation]
    ),

    onPointerUp: useCallback(() => {
      // Whatever the last chunk did not reach a threshold with still has to go
      // out, or the tail of every stroke would be visible only to its author.
      flushStrokeChunk();
      strokeChunkRef.current = null;
      isFirstPointRef.current = false;
      hasCreatedStepRef.current = false;
      onPointerRelease?.();
    }, [onPointerRelease, flushStrokeChunk]),
  };

  // Get base drawing functionality
  const baseDrawing = useBaseDrawing(
    canvasRef,
    appRef,
    drawingState,
    onHistoryChange,
    zoomLevel,
    canvasWidth,
    canvasHeight,
    onDrawingChange,
    containerRef,
    isDrawingDisabled,
    callbacks,
    !onOperation
  );
  // Filled once the hook above has built it; the callbacks handed into it read
  // this rather than a binding that does not exist yet when they are made.
  engineRef.current = baseDrawing.drawingEngine;

  /**
   * Undo, through whichever history is the authority here.
   *
   * Controlled mode has one: the canonical stream, where undo is a message
   * that every client marks and replays in the same order. The snapshot stack
   * below is the offline one, and running it as well puts a stale copy of our
   * own layers straight onto the canvas -- a local revert nobody else sees,
   * because it was never an operation. That is what made an undo here jump
   * the drawing back further than it went for anybody watching.
   */
  /**
   * Whether an undo or redo may happen right now. Every way of asking for
   * one -- the shortcut, the toolbox buttons, the handle -- comes through
   * here, so this is the one place that says no.
   *
   * Not while the host has drawing disabled: the pointer was already refused
   * then, but an undo that was not went out to the room behind the very
   * export it changed the answer to. And not while the pen is down. In a
   * session an undo sent mid-stroke was sequenced ahead of the stroke's own
   * tail, and the pointer kept drawing onto a canvas the replay had just
   * rolled back; offline it popped the stroke *before* the one in progress
   * out from under the pen. NEO answers the key mid-stroke (its
   * _keyDownHandler has no guard, and the stroke's undo step is pushed on the
   * press), which is a deliberate departure.
   */
  const mayUndo = useCallback(
    () => !isDrawingDisabled && !baseDrawing.isDrawingRef.current,
    [isDrawingDisabled, baseDrawing],
  );

  const wrappedUndo = useCallback(() => {
    if (!mayUndo()) return;
    if (onOperation) {
      onOperation({ kind: "undo", redo: false });
      return;
    }
    baseDrawing.undo();
    actionRecorderRef.current.back();
  }, [baseDrawing, mayUndo, onOperation]);

  // Redo, for the same reason and by the same rule.
  const wrappedRedo = useCallback(() => {
    if (!mayUndo()) return;
    if (onOperation) {
      onOperation({ kind: "undo", redo: true });
      return;
    }
    baseDrawing.redo();
    actionRecorderRef.current.forward();
  }, [baseDrawing, mayUndo, onOperation]);

  // Add restore action with final layer states
  const addRestoreAction = useCallback(() => {
    const engine = baseDrawing.drawingEngine;
    if (!engine) return;

    // Get both layer canvases and convert to data URLs
    const bgCanvas = engine.getLayerCanvas("background");
    const fgCanvas = engine.getLayerCanvas("foreground");

    if (bgCanvas && fgCanvas) {
      const bgDataURL = bgCanvas.toDataURL("image/png");
      const fgDataURL = fgCanvas.toDataURL("image/png");
      actionRecorderRef.current.addRestoreAction(bgDataURL, fgDataURL);
    }
  }, [baseDrawing.drawingEngine]);

  // Track if we've already initialized to prevent double-init
  const hasInitializedTwoToneRef = useRef(false);
  const hasInitializedImageRef = useRef(false);
  const initializationActionCountRef = useRef(0);

  const initializeFromImage = useCallback(async (imageUrl: string) => {
    if (hasInitializedImageRef.current) return;

    const engine = baseDrawing.drawingEngine;
    const history = baseDrawing.history;
    if (!engine || !history) return;
    hasInitializedImageRef.current = true;

    try {
      const response = await fetch(imageUrl);
      if (!response.ok) throw new Error(`Image request failed: ${response.status}`);
      const blob = await response.blob();
      const bitmap = await createImageBitmap(blob);
      const width = canvasWidth || 300;
      const height = canvasHeight || 300;
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const context = canvas.getContext("2d");
      if (!context) throw new Error("Failed to create image canvas");
      context.imageSmoothingEnabled = false;
      context.drawImage(bitmap, 0, 0, width, height);
      bitmap.close();

      engine.layers.background.set(context.getImageData(0, 0, width, height).data);
      engine.layers.foreground.fill(0);
      engine.updateAllDOMCanvasesImmediate();
      history.saveState(
        engine.layers.foreground,
        engine.layers.background,
        engine.imageWidth,
        engine.imageHeight,
        true,
      );

      actionRecorderRef.current.step();
      actionRecorderRef.current.push(
        "restore",
        canvas.toDataURL("image/png"),
        engine.getLayerCanvas("foreground")!.toDataURL("image/png"),
      );
      initializationActionCountRef.current++;
    } catch (error) {
      hasInitializedImageRef.current = false;
      throw error;
    }
  }, [baseDrawing.drawingEngine, baseDrawing.history, canvasWidth, canvasHeight]);

  // Initialize two-tone canvas with background color fill
  const initializeTwoToneCanvas = useCallback((backgroundColor: string) => {
    // Guard against double initialization
    if (hasInitializedTwoToneRef.current) return;

    const engine = baseDrawing.drawingEngine;
    const history = baseDrawing.history;
    if (!engine || !history) return;

    hasInitializedTwoToneRef.current = true;

    const bgLayer = engine.layers.background;
    const r = parseInt(backgroundColor.slice(1, 3), 16);
    const g = parseInt(backgroundColor.slice(3, 5), 16);
    const b = parseInt(backgroundColor.slice(5, 7), 16);

    // Fill entire canvas with background color (floodFill at 0,0)
    // Opacity must be in 0-255 range, not 0-1
    engine.doFloodFill(bgLayer, 0, 0, r, g, b, 255);
    engine.updateAllDOMCanvasesImmediate();

    // Save canvas state to history after fill
    // saveState takes (foreground, background) -- passing them the other way
    // round stored the fill under the foreground layer, so undoing back to this
    // entry left an opaque fill on top and hid every subsequent stroke.
    history.saveState(
      engine.layers.foreground,
      engine.layers.background,
      engine.imageWidth,
      engine.imageHeight,
      true,
    );

    // Record in replay - ABGR format
    const color = (255 << 24) | (b << 16) | (g << 8) | r;
    actionRecorderRef.current.step();
    actionRecorderRef.current.push("floodFill", 0, 0, 0, color);
    initializationActionCountRef.current++;
  }, [baseDrawing.drawingEngine, baseDrawing.history]);

  // Return enhanced interface with replay functionality
  return {
    ...baseDrawing,
    /** Hands the stroke in progress over now rather than at the next chunk. */
    flushPendingStroke: flushStrokeChunk,
    undo: wrappedUndo,
    redo: wrappedRedo,
    getReplayBlob: () =>
      actionRecorderRef.current.getReplayBlob(canvasWidth || 300, canvasHeight || 300),
    getActionCount: () => actionRecorderRef.current.getActionCount(),
    getInitializationActionCount: () => initializationActionCountRef.current,
    /** NEO's frame: ["text", layer, x, y, color, alpha, string, size, family] */
    recordText: (
      layer: "foreground" | "background",
      x: number,
      y: number,
      packedColor: number,
      alpha: number,
      text: string,
      fontSize: string,
      fontFamily: string
    ) => {
      actionRecorderRef.current.step();
      actionRecorderRef.current.push(
        "text",
        layer === "foreground" ? 1 : 0,
        x, y, packedColor, alpha, text, fontSize, fontFamily
      );
    },
    emitOperation,
    addRestoreAction,
    initializeFromImage,
    initializeTwoToneCanvas,
  };
};
