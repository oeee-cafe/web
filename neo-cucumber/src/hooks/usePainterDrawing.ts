import { useCallback, useMemo, useRef } from "react";
import { useBaseDrawing, type DrawingState } from "./useBaseDrawing";
import type { Mask } from "../neo/mask";
import type { BrushType } from "../types/drawing";
import type { PainterOperation } from "../operations";
import type { RegionTool, ToolId } from "../neo/tools";
import type { RegionRect } from "../neo/regionDrag";
import type { BezierPreviewStyle, PasteDisplay } from "../neo/regionPreview";
import type { DrawingEngine } from "../DrawingEngine";
import { clampRgba, type DrawingSink, type Layer, type Rgba } from "./drawingModes/DrawingSink";
import { ReplayRecorder } from "./drawingModes/ReplayRecorder";
import { SessionEmitter } from "./drawingModes/SessionEmitter";

export { lineTypeForBrush } from "./drawingModes/ReplayRecorder";

/**
 * What becomes of the marks. Fixed for the life of a painter: a mount is
 * either a drawing of its own or a seat in a session.
 */
export type DrawingMode =
  | {
      kind: "offline";
      /** Whether to keep a `.pch` replay; on unless the host says otherwise. */
      recordReplay?: boolean;
    }
  | {
      kind: "session";
      onOperation: (operation: PainterOperation) => void;
      /** Told when a gesture ends, for the host's pointer bookkeeping. */
      onPointerRelease?: () => void;
    };

export interface PainterDrawingOptions {
  canvasRef: React.RefObject<HTMLCanvasElement | null>;
  appRef: React.RefObject<HTMLDivElement | null>;
  drawingState: DrawingState;
  mode: DrawingMode;
  onHistoryChange?: (canUndo: boolean, canRedo: boolean) => void;
  zoomLevel?: number;
  canvasWidth?: number;
  canvasHeight?: number;
  onDrawingChange?: () => void;
  containerRef?: React.RefObject<HTMLDivElement | null>;
  isDrawingDisabled?: boolean;
  /** What the painter shows while a gesture is under way. */
  previews?: {
    /** The rubber-band rectangle while a region tool is dragged. */
    onRegionPreview?: (rect: RegionRect | null) => void;
    /** The endpoints while a straight line is dragged out. */
    onLinePreview?: (
      from: { x: number; y: number } | null,
      to: { x: number; y: number } | null,
    ) => void;
    /** The text tool was clicked; open an editor there. */
    onTextPlace?: (x: number, y: number) => void;
    /** The curve so far while a bezier is being built. */
    onBezierPreview?: (points: number[] | null, step: number, style: BezierPreviewStyle) => void;
    /** The pointer moved over the canvas, or left it. */
    onHoverMove?: (at: { x: number; y: number } | null) => void;
  };
  /** The eyedropper and the toolbox's sticky right-click button. */
  picking?: {
    /** The colour a right press found, for the host to adopt as the current one. */
    onPickColor?: (color: { r: number; g: number; b: number }) => void;
    /** Whether the toolbox's sticky right-click button is armed. */
    isVirtualRight?: () => boolean;
    /** Called when an armed press has been spent, so the button can release. */
    onVirtualRightUsed?: () => void;
  };
  /**
   * Copy and paste as NEO does them: the painter switches itself from copy
   * to paste and back, and shows the copy while it is being placed.
   */
  placement?: {
    onToolChange?: (tool: ToolId) => void;
    onPastePreview?: (display: PasteDisplay | null) => void;
  };
}

/**
 * The painter's gestures, and where their marks go.
 *
 * One drawing hook rasterises every gesture; one sink per mode records or
 * sends the result. The sink is chosen once, when the painter mounts, so
 * nothing downstream asks which mode it is in. Undo is the one thing the
 * two modes answer differently and it is answered here, in one place.
 */
export const usePainterDrawing = (options: PainterDrawingOptions) => {
  // The latest options, for callbacks that are built once and read them
  // when they run. Rebuilding the callbacks on every render rebuilt the
  // drawing hook's listeners with them.
  const optionsRef = useRef(options);
  optionsRef.current = options;

  /**
   * The engine, reachable from the sink that is handed to the hook that
   * builds it. Declared before and filled after, because the sink only ever
   * reads it later.
   */
  const engineRef = useRef<DrawingEngine | null>(null);

  const mode = options.mode;
  const kind = mode.kind;
  const sinks = useMemo(() => {
    if (kind === "session") {
      const session = new SessionEmitter(
        (operation) => {
          const current = optionsRef.current.mode;
          if (current.kind === "session") current.onOperation(operation);
        },
        () => engineRef.current,
        () => {
          const current = optionsRef.current.mode;
          if (current.kind === "session") current.onPointerRelease?.();
        },
      );
      return { sink: session as DrawingSink, session, replay: null };
    }
    const replay = new ReplayRecorder(
      mode.kind === "offline" ? (mode.recordReplay ?? true) : true,
      () => ({
        width: optionsRef.current.canvasWidth || 300,
        height: optionsRef.current.canvasHeight || 300,
      }),
    );
    return { sink: replay as DrawingSink, session: null, replay };
    // The mode is fixed for a mount; only its kind decides the sink.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kind]);
  const { sink, session, replay } = sinks;

  /** The colour and pen the paste tool stamps with, as NEO takes them. */
  const stampStyle = (): { color: Rgba; brushSize: number } => {
    const state = optionsRef.current.drawingState;
    return {
      color: {
        r: parseInt(state.color.slice(1, 3), 16),
        g: parseInt(state.color.slice(3, 5), 16),
        b: parseInt(state.color.slice(5, 7), 16),
        a: state.opacity,
      },
      brushSize: state.brushSize,
    };
  };
  const stampStyleRef = useRef(stampStyle);
  stampStyleRef.current = stampStyle;

  // Built once: every member reads the sink, or the latest options, when it
  // runs. The drawing hook re-attaches its pointer listeners whenever this
  // object changes identity, so it must not.
  const callbacks = useMemo(() => ({
    onPointerDown: () => sink.pointerDown(),
    onDrawLine: (
      fromX: number, fromY: number, toX: number, toY: number,
      brushSize: number, brushType: BrushType,
      r: number, g: number, b: number, opacity: number,
      layer: Layer, mask: Mask,
    ) => {
      if (![fromX, fromY, toX, toY].every(Number.isFinite)) {
        console.warn("Invalid coordinates in onDrawLine:", { fromX, fromY, toX, toY });
        return;
      }
      sink.segment(
        { x: Math.round(fromX), y: Math.round(fromY) },
        { x: Math.round(toX), y: Math.round(toY) },
        brushSize, brushType, clampRgba({ r, g, b, a: opacity }), layer, mask,
      );
    },
    onDrawPoint: (
      x: number, y: number, brushSize: number, brushType: BrushType,
      r: number, g: number, b: number, opacity: number,
      layer: Layer, mask: Mask,
    ) => {
      if (!Number.isFinite(x) || !Number.isFinite(y)) {
        console.warn("Invalid coordinates in onDrawPoint:", { x, y });
        return;
      }
      sink.dot(
        { x: Math.round(x), y: Math.round(y) },
        brushSize, brushType, clampRgba({ r, g, b, a: opacity }), layer, mask,
      );
    },
    onFill: (
      x: number, y: number, r: number, g: number, b: number, opacity: number,
      layer: Layer, mask: Mask,
    ): boolean => {
      if (!Number.isFinite(x) || !Number.isFinite(y)) {
        console.warn("Invalid coordinates in onFill:", { x, y });
        return false;
      }
      return sink.fill(
        { x: Math.round(x), y: Math.round(y) }, clampRgba({ r, g, b, a: opacity }), layer, mask,
      );
    },
    onEraseAll: (layer: Layer) => sink.eraseAll(layer),
    onLine: (
      from: { x: number; y: number }, to: { x: number; y: number },
      brushSize: number, brushType: BrushType, color: Rgba, layer: Layer, mask: Mask,
    ) => sink.line(from, to, brushSize, brushType, color, layer, mask),
    onBezier: (
      points: number[], brushSize: number, brushType: BrushType, color: Rgba,
      layer: Layer, mask: Mask,
    ) => sink.bezier(points, brushSize, brushType, color, layer, mask),
    onRegionCommit: (
      tool: RegionTool, layer: Layer, rect: RegionRect, color: Rgba,
      brushSize: number, mask: Mask,
    ) => sink.region(tool, layer, rect, color, brushSize, mask),
    onPaste: (layer: Layer, source: RegionRect, dx: number, dy: number, mask: Mask) => {
      const { color, brushSize } = stampStyleRef.current();
      sink.paste(layer, source, dx, dy, color, brushSize, mask);
    },
    onPointerUp: () => sink.pointerUp(),

    onRegionPreview: (rect: RegionRect | null) =>
      optionsRef.current.previews?.onRegionPreview?.(rect),
    onLinePreview: (
      from: { x: number; y: number } | null, to: { x: number; y: number } | null,
    ) => optionsRef.current.previews?.onLinePreview?.(from, to),
    onTextPlace: (x: number, y: number) => optionsRef.current.previews?.onTextPlace?.(x, y),
    onBezierPreview: (points: number[] | null, step: number, style: BezierPreviewStyle) =>
      optionsRef.current.previews?.onBezierPreview?.(points, step, style),
    onHoverMove: (at: { x: number; y: number } | null) =>
      optionsRef.current.previews?.onHoverMove?.(at),
    // The eyedropper reports a colour rather than drawing one, so it neither
    // records nor broadcasts: what it changes is which colour this person is
    // holding.
    onPickColor: (color: { r: number; g: number; b: number }) =>
      optionsRef.current.picking?.onPickColor?.(color),
    isVirtualRight: () => optionsRef.current.picking?.isVirtualRight?.() ?? false,
    onVirtualRightUsed: () => optionsRef.current.picking?.onVirtualRightUsed?.(),
    onToolChange: (tool: ToolId) => optionsRef.current.placement?.onToolChange?.(tool),
    onPastePreview: (display: PasteDisplay | null) =>
      optionsRef.current.placement?.onPastePreview?.(display),
  }), [sink]);

  const baseDrawing = useBaseDrawing(
    options.canvasRef,
    options.appRef,
    options.drawingState,
    options.onHistoryChange,
    options.zoomLevel,
    options.canvasWidth,
    options.canvasHeight,
    options.onDrawingChange,
    options.containerRef,
    options.isDrawingDisabled ?? false,
    callbacks,
    // The snapshot stack offline undo restores from. A session's undo is a
    // message every client replays, and never reads the stack.
    kind === "offline",
  );
  engineRef.current = baseDrawing.drawingEngine;

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
  const isDrawingDisabled = options.isDrawingDisabled ?? false;
  const mayUndo = useCallback(
    () => !isDrawingDisabled && !baseDrawing.isDrawingRef.current,
    [isDrawingDisabled, baseDrawing],
  );

  /**
   * Undo, through whichever history is the authority here.
   *
   * A session has one: the canonical stream, where undo is a message that
   * every client marks and replays in the same order. Offline it is the
   * snapshot stack, with the replay's head moved beside it.
   */
  const undo = useCallback(() => {
    if (!mayUndo()) return;
    if (session) {
      session.undo(false);
      return;
    }
    baseDrawing.undo();
    replay?.undo();
  }, [baseDrawing, mayUndo, session, replay]);

  const redo = useCallback(() => {
    if (!mayUndo()) return;
    if (session) {
      session.undo(true);
      return;
    }
    baseDrawing.redo();
    replay?.redo();
  }, [baseDrawing, mayUndo, session, replay]);

  // Track if we've already initialized to prevent double-init
  const hasInitializedTwoToneRef = useRef(false);
  const hasInitializedImageRef = useRef(false);

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
      const width = optionsRef.current.canvasWidth || 300;
      const height = optionsRef.current.canvasHeight || 300;
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
      replay?.recordOpeningRestore(
        canvas.toDataURL("image/png"),
        engine.getLayerCanvas("foreground")!.toDataURL("image/png"),
      );
    } catch (error) {
      hasInitializedImageRef.current = false;
      throw error;
    }
  }, [baseDrawing.drawingEngine, baseDrawing.history, replay]);

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
    replay?.recordOpeningFill((255 << 24) | (b << 16) | (g << 8) | r);
  }, [baseDrawing.drawingEngine, baseDrawing.history, replay]);

  return {
    ...baseDrawing,
    undo,
    redo,
    /** The `.pch` being recorded; null in a session, which records none. */
    replay,
    /** The room's stream; null offline. */
    session,
    initializeFromImage,
    initializeTwoToneCanvas,
  };
};
