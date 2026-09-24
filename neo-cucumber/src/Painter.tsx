import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useCanvasView, useDeferredHandler } from "./hooks/useCanvasView";
import "./App.css";
import { useLingui } from "@lingui/react/macro";
import { ToolboxPanels } from "./components/ToolboxPanels";
import { NARROW_WORKSPACE, TOOLBOX_LANE } from "./components/toolboxAnchor";
import { NEO_BUTTON } from "./components/neo/neoClasses";
import { ALL_TOOLS } from "./constants/drawing";
import { SimplifiedToolbox } from "./components/SimplifiedToolbox";
import { useOfflineDrawing } from "./hooks/useOfflineDrawing";
import { useDrawingState } from "./hooks/useDrawingState";
import { usePalettePresets } from "./hooks/usePalettePresets";
import { useDrawingTimer } from "./hooks/useDrawingTimer";
import { useTwoToneShortcuts } from "./hooks/useTwoToneShortcuts";
import { usePainterShortcuts } from "./hooks/usePainterShortcuts";
import { preferPen } from "./utils/penPreference";
import { ShortcutHelp } from "./components/ShortcutHelp";
import type { ShortcutAction } from "./constants/shortcuts";
import { MAX_BRUSH_SIZE, MIN_BRUSH_SIZE } from "./constants/drawing";
import { useZoomControls } from "./hooks/useZoomControls";
import { usePinchZoom } from "./hooks/usePinchZoom";
import { useOfflineCanvas } from "./hooks/useOfflineCanvas";
import { compositeLayersToCanvas } from "./utils/canvasExport";
import {
  type Backdrop,
  type BezierPreviewStyle,
  drawBezierPreview,
  drawLinePreview,
  drawPastePreview,
  drawRegionPreview,
  type PasteDisplay,
} from "./neo/regionPreview";
import type { DrawingEngine } from "./DrawingEngine";
import type { RegionRect } from "./neo/regionDrag";
import { TEXT_FONT_FAMILY, fontSizeForBrush, type ToolId } from "./neo/tools";
import { previewBackdrop as backdropFromCanvasStack } from "./neo/previewBackdrop";
import { inJoinOrder } from "./neo/canvasStack";
import { PainterWorkspace } from "./components/PainterWorkspace";
import type {
  CanonicalPainterOperation,
  ImageSource,
  PainterCheckpoint,
  PainterExport,
  PainterError,
  PainterCommand,
  PainterHandle,
  PainterOptions,
  PainterSessionArchive,
} from "./public";
import { CanvasHistory } from "./utils/canvasHistory";
import { paintThumbnail } from "./utils/thumbnail";
import { layerToPngBlob, pngDataToLayer } from "./utils/canvasSnapshot";

interface PainterProps {
  config: PainterOptions;
}

function canvasToPng(canvas: HTMLCanvasElement): Promise<Blob> {
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => {
      if (blob) resolve(blob);
      else reject(new Error("Failed to encode canvas as PNG"));
    }, "image/png");
  });
}

const Painter = forwardRef<PainterHandle, PainterProps>(function Painter(
  { config },
  ref,
) {
  const { t } = useLingui();

  // The reusable painter receives its complete configuration from its host.
  const {
    canvasWidth,
    canvasHeight,
    controls,
    twoToneConfig,
  } =
    useMemo(() => {
      const resolved = config;
      const width = resolved.width;
      const height = resolved.height;
      const twoToneConfig =
        resolved.mode.kind === "two-tone"
          ? {
              enabled: true,
              backgroundColor: resolved.mode.backgroundColor,
              foregroundColor: resolved.mode.foregroundColor,
            }
          : null;

      return {
        canvasWidth: width,
        canvasHeight: height,
        controls: resolved.controls,
        twoToneConfig,
      };
    }, [config]);

  const {
    drawingState,
    selectedPaletteIndex,
    paletteColors,
    setDrawingState,
    setSelectedPaletteIndex,
    setPaletteColors,
    updateBrushType,
    updateColor,
    setPaletteColor,
    initializeForTwoTone,
    selectPen,
    swapPen,
    setPenSize,
    adjustPenSize,
  } = useDrawingState();
  const palettePresets = usePalettePresets(config.palettePresets);

  // Initialize two-tone palette immediately if in two-tone mode
  // Use useLayoutEffect to ensure synchronous execution before first paint
  useLayoutEffect(() => {
    if (twoToneConfig) {
      initializeForTwoTone(
        twoToneConfig.backgroundColor,
        twoToneConfig.foregroundColor
      );
    }
  }, [twoToneConfig, initializeForTwoTone]);

  const [historyState, setHistoryState] = useState({
    canUndo: false,
    canRedo: false,
  });
  const [interactionEnabled, setInteractionEnabled] = useState(true);
  /**
   * Which participants this viewer has hidden, and whose layers new marks go
   * into. Both are this screen's business: hiding is a way of looking at the
   * drawing rather than an edit, so neither is broadcast and neither reaches
   * the saved image.
   */
  const [hiddenOwners, setHiddenOwners] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const [targetOwner, setTargetOwner] = useState<string | null>(null);
  /** Where the host would like the layers window, if it has an opinion. */
  const [layersOrigin, setLayersOrigin] = useState<{ x: number; y: number } | null>(null);
  const selectTargetOwner = useCallback((actorId: string) => {
    setTargetOwner(actorId);
  }, []);
  /**
   * Tells the painter what to call the participants.
   *
   * It knows every actor that has drawn, because their layers exist, but not
   * who any of them are -- names and colours belong to the host's roster.
   */
  const setParticipants = useCallback(
    (participants: { actorId: string; name: string; color?: string }[]) => {
      setParticipantNames(
        new Map(
          participants.map(({ actorId, name, color }) => [actorId, { name, color }]),
        ),
      );
    },
    [],
  );
  /**
   * Composites a participant's pair into their row's thumbnail.
   *
   * Drawn from their mounted canvases rather than their buffers: those are
   * already what is on screen, and reading the buffers would mean copying a
   * whole layer into an ImageData for every participant on every refresh.
   */
  // Keyed by the thumbnail itself, not by whose it is. A row's canvas can be
  // replaced -- the layers window closing and opening again, a participant
  // re-keyed -- and a fresh canvas is blank whatever was painted into the
  // last one, so remembering it per participant left it blank until they
  // next drew.
  const thumbnailGenerations = useRef(new WeakMap<HTMLCanvasElement, string>());
  const drawThumbnail = useCallback(
    (actorId: string, target: HTMLCanvasElement) => {
      const engine = drawingEngineRef.current;
      if (!engine) return;
      // Nothing to redraw for somebody who has stopped drawing. Scaling two
      // full-size canvases down is real work, it runs for every participant on
      // a timer, and in a room where one person is drawing it was redrawing
      // everybody else's unchanged picture alongside theirs. The engine counts
      // every write to a participant's layers, so it can say.
      const generation = engine.layerGeneration(actorId);
      const painted = `${actorId}:${target.width}x${target.height}:${generation}`;
      if (thumbnailGenerations.current.get(target) === painted) return;
      thumbnailGenerations.current.set(target, painted);
      // A participant who has not drawn has no canvas yet, and blank paper
      // is the honest picture of that.
      const sources = (["background", "foreground"] as const)
        .map((layer) => engine.domCanvasFor(layer, actorId))
        .filter((source): source is HTMLCanvasElement => source !== undefined);
      paintThumbnail(target, sources);
    },
    [],
  );
  const toggleOwnerVisible = useCallback((actorId: string) => {
    setHiddenOwners((current) => {
      const next = new Set(current);
      if (!next.delete(actorId)) next.add(actorId);
      return next;
    });
  }, []);
  /** Everyone with layers, in the order they composite. */
  const [layerOwners, setLayerOwners] = useState<string[]>([]);
  /** Display names the host knows and the painter does not. */
  const [participantNames, setParticipantNames] = useState<
    ReadonlyMap<string, { name: string; color?: string }>
  >(() => new Map());
  /**
   * Whether the opening zoom has been applied.
   *
   * The toolboxes wait for it. They open beside the drawing, and until this is
   * done the drawing is whatever size it was declared at rather than the size
   * it will be shown at -- on a phone that is a 300px canvas about to become
   * 212px, and panels placed beside the wrong one land off the screen.
   */
  const [openedAtFittedZoom, setOpenedAtFittedZoom] = useState(false);
  const operationCounterRef = useRef(0);
  const synchronizationHistoryRef = useRef<CanvasHistory | null>(null);
  /**
   * The checkpoint the canonical log is relative to, for the session archive.
   *
   * A reset point leaves only its sequence here: the pixels are the history's
   * base savepoint, and they are encoded if an archive is ever asked for.
   */
  const appliedCheckpointRef = useRef<
    PainterCheckpoint | { sequence: number; encoded?: undefined } | undefined
  >(undefined);
  const synchronization = config.synchronization;
  /**
   * Who this painter is on the canonical stream. Mounting has to name someone
   * before the server has spoken, so hosts whose stream is keyed by a
   * server-assigned id replace it through `setLocalActorId` once they know it.
   */
  const localActorIdRef = useRef(synchronization?.actorId ?? "");
  const setLocalActorId = useCallback((actorId: string) => {
    localActorIdRef.current = actorId;
    synchronizationHistoryRef.current?.setLocalUserId(actorId);
    // The engine keys layer pairs by actor too. Leaving its pair under the
    // old name would give one person two of them: the one they have been
    // drawing into, and a second one the canonical stream addresses.
    drawingEngineRef.current?.setLocalOwner(actorId);
  }, []);
  /**
   * Sends a local mark to the canonical stream, or is absent when there is no
   * stream to send to.
   *
   * Absent, rather than a callback that quietly drops what it is given: the
   * drawing hook reads this as "somebody else owns undo" -- in a session undo
   * is a message every client replays, offline it is the snapshot stack. A
   * do-nothing callback claimed a stream that was not there, so undo in a
   * plain painter sent an operation into the void instead of reverting.
   */
  const emitLocalOperation = useMemo(() => {
    if (!synchronization) return undefined;
    return (operation: import("./operations").PainterOperation) => {
      operationCounterRef.current += 1;
      // A mark aimed at somebody else's pair has to say so, or every other
      // client will put it in ours -- where it did not happen.
      const target = targetOwnerRef.current;
      const aimed =
        target && target !== localActorIdRef.current && "layer" in operation
          ? { ...operation, targetActorId: target }
          : operation;
      const entry = {
        id: `${localActorIdRef.current}:${operationCounterRef.current}`,
        actorId: localActorIdRef.current,
        operation: aimed,
        timestamp: Date.now(),
      };
      synchronizationHistoryRef.current?.registerOptimisticOperation(entry);
      synchronization.onOperation(entry);
    };
  }, [synchronization]);
  const targetOwnerRef = useRef<string | null>(null);

  /**
   * The participants the layer toolbox lists, top of the stack first, so the
   * rows read down the screen the way the layers stack on it.
   */
  const participantLayers = useMemo(
    () =>
      // Everyone in the room, not only everyone who has drawn. A pair comes
      // into being the first time an operation names its owner, so listing
      // just those would leave somebody who has joined and not yet made a
      // mark out of the toolbox -- and they are exactly who you want to be
      // able to see, hide, or draw for.
      inJoinOrder([...new Set([...layerOwners, ...participantNames.keys()])]).map(
        (actorId) => ({
          actorId,
          name: participantNames.get(actorId)?.name ?? actorId,
          color: participantNames.get(actorId)?.color,
        }),
      ),
    [layerOwners, participantNames],
  );

  /**
   * NEO's Right Click button: armed by a press, spent by the next one.
   *
   * A right press is the eyedropper, and a tablet or a trackpad may have no
   * way to make one. NEO answers that with a button that says "the next press
   * is a right press" and releases itself afterwards, which is what this is.
   */
  const [virtualRight, setVirtualRight] = useState(false);
  const virtualRightRef = useRef(false);
  useEffect(() => { virtualRightRef.current = virtualRight; }, [virtualRight]);
  const isVirtualRight = useCallback(() => virtualRightRef.current, []);
  const releaseVirtualRight = useCallback(() => setVirtualRight(false), []);
  const toggleVirtualRight = useCallback(() => setVirtualRight((on) => !on), []);
  const adoptPickedColor = useCallback(
    (color: { r: number; g: number; b: number }) => {
      const hex = (value: number) => value.toString(16).padStart(2, "0");
      updateColor(`#${hex(color.r)}${hex(color.g)}${hex(color.b)}`);
    },
    [updateColor],
  );

  const appRef = useRef<HTMLDivElement>(null);
  /**
   * The rubber band a region tool is being dragged out over. Its own canvas
   * above the layers: it is a cursor, not part of the drawing, so it must not
   * touch the pixels or be recorded.
   */
  const previewCanvasRef = useRef<HTMLCanvasElement>(null);
  const previewEngineRef = useRef<DrawingEngine | null>(null);
  const previewBackdrop = useCallback((): Backdrop | null => {
    const engine = previewEngineRef.current;
    if (!engine) return null;
    return backdropFromCanvasStack(
      engine,
      canvasWidth,
      canvasHeight,
      drawingState.zoomLevel / 100,
      drawingState.bgVisible,
      drawingState.fgVisible
    );
  }, [canvasWidth, canvasHeight, drawingState.zoomLevel, drawingState.bgVisible, drawingState.fgVisible]);
  const handleRegionPreview = useCallback((rect: RegionRect | null) => {
    const ctx = previewCanvasRef.current?.getContext("2d");
    if (ctx) drawRegionPreview(ctx, rect, previewBackdrop(), drawingState.brushType);
  }, [previewBackdrop, drawingState.brushType]);
  const handlePastePreview = useCallback((display: PasteDisplay | null) => {
    const ctx = previewCanvasRef.current?.getContext("2d");
    if (ctx) drawPastePreview(ctx, display, previewBackdrop());
  }, [previewBackdrop]);
  /**
   * Where the text tool was clicked, if an editor is open there. NEO puts an
   * editable box straight on the canvas rather than in a dialog: you type in
   * place, Enter commits and Escape abandons. There are no font controls
   * because the pen size is the font size and the family is fixed.
   */
  const [textAt, setTextAt] = useState<{ x: number; y: number } | null>(null);
  const textBoxRef = useRef<HTMLDivElement>(null);
  const handleTextPlace = useCallback((x: number, y: number) => {
    setTextAt({ x, y });
  }, []);

  const handleLinePreview = useCallback(
    (
      from: { x: number; y: number } | null,
      to: { x: number; y: number } | null
    ) => {
      const ctx = previewCanvasRef.current?.getContext("2d");
      if (ctx) drawLinePreview(ctx, from, to, previewBackdrop());
    },
    [previewBackdrop]
  );
  const handleBezierPreview = useCallback(
    (points: number[] | null, step: number, style: BezierPreviewStyle) => {
      const ctx = previewCanvasRef.current?.getContext("2d");
      if (ctx) drawBezierPreview(ctx, points, previewBackdrop(), step, style);
    },
    [previewBackdrop]
  );

  /*
   * The cursor is painted by useCanvasView, which needs the engine that the
   * drawing hook below creates -- so the hook is given a forwarder now and
   * pointed at the real painter once both exist.
   */
  const [handleHoverMove, setHoverHandler] = useDeferredHandler<
    { x: number; y: number } | null
  >();
  /**
   * Whether a stroke is in progress, from the drawing hook below -- pointed at
   * its flag once that exists, the same way the cursor painter above is.
   */
  const strokeActiveRef = useRef<{ current: boolean } | null>(null);
  const handleSynchronizedHover = useCallback(
    (at: { x: number; y: number } | null) => {
      // The local cursor is painted from every move, always: it is this
      // screen's own pointer and it has to keep up with the pen.
      handleHoverMove(at);
      // The room is told only while the pen is up. A stroke already says where
      // the pen is -- each chunk carries the points it drew, and that is what
      // moves this user's cursor on everybody else's screen -- so a pointer
      // message sent during one is the same position again, at a second rate,
      // fanned out to every participant and decoded by all of them. Drawpile
      // draws the same line: its canvas view emits `pointerMoved` only in the
      // branch where the pen is not down.
      // `null` is "the pointer is gone", not a position, and always goes out:
      // it is what retires this user's cursor on the other screens.
      if (at !== null && strokeActiveRef.current?.current) return;
      synchronization?.onPointerMove?.(at);
    },
    [handleHoverMove, synchronization],
  );

  const tempCanvasContainerRef = useRef<HTMLDivElement>(null);
  const tempLocalUserCanvasRef = useRef<HTMLCanvasElement>(null);

  /**
   * Publishes the undo/redo flags, and only when they have actually moved.
   *
   * The history calls this after every operation it sees -- ours, and every
   * message the canonical stream delivers. A room of three sends some eighty
   * of those a second, and a fresh `{ canUndo, canRedo }` for each one is
   * eighty re-renders of the whole painter: both toolbox columns, every tool
   * button, the sliders, the participant list. The flags themselves change a
   * handful of times in a session. Returning the previous object when nothing
   * moved lets React drop the update at the dispatch, so the canonical stream
   * costs a canvas write and nothing else.
   */
  const handleHistoryChange = useCallback(
    (canUndo: boolean, canRedo: boolean) => {
      setHistoryState((previous) =>
        previous.canUndo === canUndo && previous.canRedo === canRedo
          ? previous
          : { canUndo, canRedo }
      );
    },
    []
  );
  /**
   * The offline history's answer, which only counts when it is the authority.
   *
   * There are two histories and one pair of buttons. In a session the
   * canonical stream is in charge -- undo there is a message every client
   * marks and replays -- and the offline snapshot stack is empty, so its
   * answer is "nothing to undo" over the top of the real one. Offline it is
   * the only history there is.
   *
   * Read through a ref rather than closed over, so this callback's identity
   * never changes: it is handed to the drawing hook, and a new one on every
   * render is a new reason for everything downstream of it to re-run.
   */
  const controlledRef = useRef(false);
  controlledRef.current = Boolean(synchronization);
  const handleOfflineHistoryChange = useCallback(
    (canUndo: boolean, canRedo: boolean) => {
      if (controlledRef.current) return;
      handleHistoryChange(canUndo, canRedo);
    },
    [handleHistoryChange]
  );

  // Create a ref to hold the DOM canvas update function
  const domCanvasUpdateRef = useRef<() => void>(() => {});


  // A finished copy switches the painter to paste and a paste switches it
  // back, NEO's CopyTool/PasteTool hand-off; the preview shows the copy while
  // it is being placed.
  const placement = useMemo(
    () => ({ onToolChange: updateBrushType, onPastePreview: handlePastePreview }),
    [updateBrushType, handlePastePreview]
  );

  // Use the offline drawing hook
  const {
    undo,
    redo,
    drawingEngine,
    getReplayBlob,
    getActionCount,
    getInitializationActionCount,
    addRestoreAction,
    initializeFromImage,
    initializeTwoToneCanvas,
    recordText,
    emitOperation,
    isDrawingRef,
    setInteractionSuspended,
    flushPendingStroke,
  } = useOfflineDrawing(
    tempLocalUserCanvasRef,
    appRef,
    drawingState,
    handleOfflineHistoryChange,
    drawingState.zoomLevel,
    canvasWidth,
    canvasHeight,
    // Nothing to do per segment. Every kernel the engine runs queues a repaint
    // of the rectangle it wrote, uploaded once a frame; this used to re-upload
    // every participant's two layers whole, synchronously, for every segment
    // of the local stroke -- which made a stroke cost more with each person in
    // the room, and on a large canvas was most of the frame.
    undefined,
    tempCanvasContainerRef,
    handleRegionPreview,
    handleLinePreview,
    handleTextPlace,
    handleBezierPreview,
    handleSynchronizedHover,
    !interactionEnabled,
    emitLocalOperation,
    synchronization?.onPointerUp,
    adoptPickedColor,
    isVirtualRight,
    releaseVirtualRight,
    config.recordReplay ?? true,
    placement,
  );
  previewEngineRef.current = drawingEngine ?? null;
  const flushPendingStrokeRef = useRef(flushPendingStroke);
  flushPendingStrokeRef.current = flushPendingStroke;
  strokeActiveRef.current = isDrawingRef;

  useEffect(() => {
    if (!drawingEngine || !synchronization) {
      synchronizationHistoryRef.current = null;
      return;
    }
    const history = new CanvasHistory(drawingEngine, handleHistoryChange);
    history.setLocalUserId(localActorIdRef.current);
    history.setLocalWork({
      drawing: () => isDrawingRef.current,
      flush: () => flushPendingStrokeRef.current(),
    });
    synchronizationHistoryRef.current = history;
    return () => {
      if (synchronizationHistoryRef.current === history) {
        synchronizationHistoryRef.current = null;
      }
    };
  }, [drawingEngine, synchronization, handleHistoryChange, isDrawingRef]);

  const readinessRef = useRef<{
    promise: Promise<void>;
    resolve: () => void;
  } | null>(null);
  if (!readinessRef.current) {
    let resolve = () => {};
    const promise = new Promise<void>((done) => {
      resolve = done;
    });
    readinessRef.current = { promise, resolve };
  }
  /**
   * Ready means the controls are up too, not just the canvases.
   *
   * The toolbox waits for the opening zoom, because it opens beside a drawing
   * whose size that zoom decides. A host is entitled to find the toolbox in the
   * DOM once this resolves -- both of ours reach into it, to move a Save button
   * into the panel where it belongs -- so resolving before the panels exist
   * turned that into "could not find the extra toolbox actions".
   */
  const controlsReady = controls.kind === "none" || openedAtFittedZoom;
  useEffect(() => {
    if (drawingEngine && controlsReady) readinessRef.current?.resolve();
  }, [drawingEngine, controlsReady]);

  const actionCount = getActionCount();
  const onChange = config?.onChange;
  useEffect(() => {
    onChange?.({
      canUndo: historyState.canUndo,
      canRedo: historyState.canRedo,
      strokeCount: actionCount,
      dirty: actionCount > 0,
    });
  }, [onChange, historyState.canUndo, historyState.canRedo, actionCount]);

  // Focus the box as soon as it appears, so typing just works
  useEffect(() => {
    if (textAt && textBoxRef.current) {
      textBoxRef.current.textContent = "";
      textBoxRef.current.focus();
    }
  }, [textAt]);

  /** Enter commits the text, Escape abandons it -- NEO's keys. */
  const handleTextKey = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setTextAt(null);
        return;
      }
      if (e.key !== "Enter" || e.shiftKey) return;
      e.preventDefault();

      const value = textBoxRef.current?.textContent ?? "";
      if (!textAt || !drawingEngine || !value) {
        setTextAt(null);
        return;
      }

      const r = parseInt(drawingState.color.slice(1, 3), 16);
      const g = parseInt(drawingState.color.slice(3, 5), 16);
      const b = parseInt(drawingState.color.slice(5, 7), 16);
      const size = `${fontSizeForBrush(drawingState.brushSize)}px`;

      drawingEngine.drawText(
        drawingState.layerType,
        textAt.x,
        textAt.y,
        { r, g, b },
        drawingState.opacity / 255,
        value,
        size,
        TEXT_FONT_FAMILY
      );
      // NEO packs the colour with red in the low byte
      recordText(
        drawingState.layerType,
        textAt.x,
        textAt.y,
        r | (g << 8) | (b << 16),
        drawingState.opacity / 255,
        value,
        size,
        TEXT_FONT_FAMILY
      );
      emitOperation({
        kind: "text",
        layer: drawingState.layerType,
        at: textAt,
        text: value,
        color: { r, g, b, a: drawingState.opacity },
        brushSize: drawingState.brushSize,
        mask: {
          type: drawingState.maskType ?? 0,
          r: parseInt((drawingState.maskColor ?? "#000000").slice(1, 3), 16),
          g: parseInt((drawingState.maskColor ?? "#000000").slice(3, 5), 16),
          b: parseInt((drawingState.maskColor ?? "#000000").slice(5, 7), 16),
        },
      });
      domCanvasUpdateRef.current();
      setTextAt(null);
    },
    [textAt, drawingEngine, drawingState, recordText, emitOperation]
  );

  // Zoom controls
  const {
    currentZoom,
    handleZoomIn,
    handleZoomOut,
    handleZoomReset,
    handleZoomFit,
    zoomToFit,
    zoomToScale,
  } = useZoomControls({
    canvasContainerRef: tempCanvasContainerRef,
    appRef,
    drawingEngine,
    setDrawingState,
  });

  // Two fingers on the drawing are a way of looking at it, not of marking it
  usePinchZoom({
    appRef,
    canvasContainerRef: tempCanvasContainerRef,
    drawingEngine,
    currentZoom,
    zoomToScale,
    setInteractionSuspended,
  });

  // Canvas management
  const { canvasContainerRef } = useOfflineCanvas({
    canvasWidth,
    canvasHeight,
    drawingEngine,
    hiddenOwners,
    bgVisible: drawingState.bgVisible,
    fgVisible: drawingState.fgVisible,
  });

  const { cursorCanvasRef, paintCursor } = useCanvasView({
    drawingEngine,
    drawingState,
    canvasContainerRef,
    currentZoom,
    canvasWidth,
    canvasHeight,
  });
  setHoverHandler(paintCursor);

  const exportPng = useCallback(async (): Promise<Blob> => {
    if (!drawingEngine) throw new Error("Painter is not ready");
    // Every participant's pair, bottom of the stack first, and everyone is in
    // it: hiding somebody is a way of looking at the drawing, not an edit.
    const layers: HTMLCanvasElement[] = [];
    for (const owner of inJoinOrder(drawingEngine.ownerIds()).reverse()) {
      for (const layer of ["background", "foreground"] as const) {
        const canvas = drawingEngine.getLayerCanvas(layer, owner);
        if (canvas) layers.push(canvas);
      }
    }
    const composited = compositeLayersToCanvas(canvasWidth, canvasHeight, layers);
    if (!composited) throw new Error("Failed to composite canvas layers");
    return canvasToPng(composited);
  }, [drawingEngine, canvasWidth, canvasHeight]);

  const exportReplay = useCallback(async (): Promise<Blob> => {
    if (!drawingEngine) throw new Error("Painter is not ready");
    if (config.recordReplay === false) {
      const error = new Error(
        "This painter was mounted without replay recording",
      ) as PainterError;
      error.code = "export-failed";
      throw error;
    }
    addRestoreAction();
    return getReplayBlob();
  }, [drawingEngine, addRestoreAction, getReplayBlob, config.recordReplay]);

  const save = useCallback(async (): Promise<PainterExport> => {
    // Start both captures in the same JavaScript turn. exportPng snapshots the
    // composited pixels and exportReplay snapshots the action list before
    // either promise yields back to pointer input.
    const pngPromise = exportPng();
    const replayPromise = exportReplay();
    const [png, replay] = await Promise.all([pngPromise, replayPromise]);
    const nonStrokeActions = 1 + getInitializationActionCount();
    return {
      png,
      replay,
      width: canvasWidth,
      height: canvasHeight,
      strokeCount: Math.max(0, getActionCount() - nonStrokeActions),
    };
  }, [
    exportPng,
    exportReplay,
    canvasWidth,
    canvasHeight,
    getActionCount,
    getInitializationActionCount,
  ]);

  const loadImage = useCallback(
    async (source: ImageSource): Promise<void> => {
      let objectUrl: string | null = null;
      try {
        const url =
          source instanceof Blob
            ? (objectUrl = URL.createObjectURL(source))
            : source instanceof URL
              ? source.href
              : source;
        await initializeFromImage(url);
      } finally {
        if (objectUrl) URL.revokeObjectURL(objectUrl);
      }
    },
    [initializeFromImage],
  );

  const applyCanonicalOperation = useCallback(
    async (operation: CanonicalPainterOperation): Promise<void> => {
      const history = synchronizationHistoryRef.current;
      if (!history) throw new Error("Painter is not in controlled mode");
      await history.handleCanonicalOperation(operation);
      // No repaint is forced here. The history queues one for the region it
      // drew, and the engine coalesces those to a frame -- where a forced
      // repaint per message would cancel that batch and upload both layers
      // whole, which costs more than applying the message did. A busy room
      // sends a message per pointer move, per participant.
    },
    [],
  );

  const exportCheckpoint = useCallback(
    async (sequence: number): Promise<PainterCheckpoint> => {
      if (!drawingEngine) throw new Error("Painter is not ready");
      if (!Number.isSafeInteger(sequence) || sequence < 0) {
        throw new Error("Checkpoint sequence must be a non-negative safe integer");
      }
      const layers = await Promise.all(
        drawingEngine.ownerIds().map(async (actorId) => {
          const pair = drawingEngine.layersFor(actorId);
          const [background, foreground] = await Promise.all([
            layerToPngBlob(pair.background, canvasWidth, canvasHeight),
            layerToPngBlob(pair.foreground, canvasWidth, canvasHeight),
          ]);
          return { actorId, background, foreground };
        }),
      );
      return { sequence, width: canvasWidth, height: canvasHeight, layers };
    },
    [drawingEngine, canvasWidth, canvasHeight],
  );

  const applyCheckpoint = useCallback(
    async (checkpoint: PainterCheckpoint): Promise<void> => {
      if (!drawingEngine) throw new Error("Painter is not ready");
      if (checkpoint.width !== canvasWidth || checkpoint.height !== canvasHeight) {
        throw new Error(
          `Checkpoint is ${checkpoint.width}x${checkpoint.height}; painter is ${canvasWidth}x${canvasHeight}`,
        );
      }
      const decoded = await Promise.all(
        checkpoint.layers.map(async (entry) => ({
          actorId: entry.actorId,
          background: await pngDataToLayer(
            new Uint8Array(await entry.background.arrayBuffer()), canvasWidth, canvasHeight),
          foreground: await pngDataToLayer(
            new Uint8Array(await entry.foreground.arrayBuffer()), canvasWidth, canvasHeight),
        })),
      );
      // A checkpoint is the whole canvas, so anyone it does not mention has no
      // work in it and must be blanked rather than left holding stale pixels.
      const named = new Set(decoded.map((entry) => entry.actorId));
      for (const actorId of drawingEngine.ownerIds()) {
        if (named.has(actorId)) continue;
        const pair = drawingEngine.layersFor(actorId);
        pair.background.fill(0);
        pair.foreground.fill(0);
      }
      for (const entry of decoded) {
        const pair = drawingEngine.layersFor(entry.actorId);
        pair.background.set(entry.background);
        pair.foreground.set(entry.foreground);
      }
      synchronizationHistoryRef.current?.reset();
      appliedCheckpointRef.current = checkpoint;
      domCanvasUpdateRef.current();
    },
    [drawingEngine, canvasWidth, canvasHeight],
  );

  /**
   * The applied checkpoint as PNGs, encoding a reset point's on first ask.
   *
   * Read before anything is awaited, so the canonical log returned beside it
   * is relative to the same point; and the base savepoint is read in the
   * same turn, before another reset could replace it.
   */
  const encodedCheckpoint = useCallback(
    async (history: CanvasHistory): Promise<PainterCheckpoint | undefined> => {
      const applied = appliedCheckpointRef.current;
      if (!applied || "layers" in applied) return applied;
      const base = [...history.baseLayers()];
      const checkpoint: PainterCheckpoint = {
        sequence: applied.sequence,
        width: canvasWidth,
        height: canvasHeight,
        layers: await Promise.all(
          base.map(async ([actorId, pair]) => {
            const [background, foreground] = await Promise.all([
              layerToPngBlob(pair.background, canvasWidth, canvasHeight),
              layerToPngBlob(pair.foreground, canvasWidth, canvasHeight),
            ]);
            return { actorId, background, foreground };
          }),
        ),
      };
      // Kept, so a second export does not encode it again -- unless a newer
      // checkpoint arrived while this one was encoding.
      if (appliedCheckpointRef.current === applied) {
        appliedCheckpointRef.current = checkpoint;
      }
      return checkpoint;
    },
    [canvasWidth, canvasHeight],
  );

  const exportSessionArchive = useCallback(async (): Promise<PainterSessionArchive> => {
    if (!drawingEngine) throw new Error("Painter is not ready");
    const history = synchronizationHistoryRef.current;
    if (!history) throw new Error("Painter is not in controlled mode");
    return {
      format: "neo-cucumber-session",
      version: 1,
      canvas: { width: canvasWidth, height: canvasHeight, mode: config.mode },
      checkpoint: await encodedCheckpoint(history),
      operations: history.getCanonicalOperations(),
    };
  }, [drawingEngine, canvasWidth, canvasHeight, config.mode, encodedCheckpoint]);

  const synchronizationTrace = useCallback(
    () => synchronizationHistoryRef.current?.synchronizationTrace() ?? [],
    [],
  );

  const compactCanonicalHistory = useCallback(async (sequence: number): Promise<void> => {
    const history = synchronizationHistoryRef.current;
    if (!history) throw new Error("Painter is not in controlled mode");
    if (!(await history.handleResetPoint(sequence))) return;
    // Only the sequence. Every client in the room compacts at every reset
    // point, and encoding each participant's pair to PNG here -- for an
    // archive that is almost never exported -- was the hitch the whole room
    // felt every five hundred messages.
    appliedCheckpointRef.current = { sequence };
  }, []);

  const isSynchronizationSettled = useCallback((): boolean =>
    !isDrawingRef.current && !(synchronizationHistoryRef.current?.hasPendingLocal ?? false),
  [isDrawingRef]);

  /**
   * The tool before the one in hand, for a pen's "switch to previous" and for
   * leaving the eraser. Only ever a different tool from the current one.
   */
  const previousToolRef = useRef<ToolId | null>(null);
  const currentToolRef = useRef<ToolId>(drawingState.brushType);
  useEffect(() => {
    if (drawingState.brushType === currentToolRef.current) return;
    previousToolRef.current = currentToolRef.current;
    currentToolRef.current = drawingState.brushType;
  }, [drawingState.brushType]);

  const command = useCallback(
    (name: PainterCommand) => {
      if (twoToneConfig !== null) return;
      const current = currentToolRef.current;
      const previous = previousToolRef.current;
      if (name === "toggle-eraser") {
        updateBrushType(current === "eraser" ? previous ?? "solid" : "eraser");
      } else if (name === "previous-tool" && previous) {
        updateBrushType(previous);
      }
    },
    [twoToneConfig, updateBrushType]
  );

  useImperativeHandle(
    ref,
    () => ({
      ready: readinessRef.current!.promise,
      save,
      exportPng,
      exportReplay,
      loadImage,
      undo,
      redo,
      command,
      preferPen,
      setInteractionEnabled,
      setLocalActorId,
      setParticipants,
      setLayersOrigin,
      applyCanonicalOperation,
      exportCheckpoint,
      applyCheckpoint,
      exportSessionArchive,
      synchronizationTrace,
      compactCanonicalHistory,
      isSynchronizationSettled,
      // The owning mount adapter replaces this with its React-root teardown.
      unmount: () => {},
    }),
    [save, exportPng, exportReplay, loadImage, undo, redo, command, setLocalActorId, setParticipants, setLayersOrigin, applyCanonicalOperation, exportCheckpoint, applyCheckpoint, exportSessionArchive, compactCanonicalHistory, isSynchronizationSettled, synchronizationTrace],
  );

  // Point the engine, the history and the emitter at the selected participant
  useEffect(() => {
    targetOwnerRef.current = targetOwner;
    drawingEngine?.setDrawTarget(targetOwner);
    if (targetOwner) {
      synchronizationHistoryRef.current?.setLocalTargetOwner(targetOwner);
    }
  }, [drawingEngine, targetOwner]);

  // Follow the participants the engine knows about
  useEffect(() => {
    if (!drawingEngine) return;
    const refresh = () => setLayerOwners(inJoinOrder(drawingEngine.ownerIds()));
    refresh();
    return drawingEngine.onOwnersChanged(refresh);
  }, [drawingEngine]);

  // Keep drawingEngine ref in sync
  const drawingEngineRef = useRef(drawingEngine);
  useEffect(() => {
    drawingEngineRef.current = drawingEngine;
    // The engine is built before anyone has named this painter, so its pair
    // starts under a placeholder. Give it the actor's name as soon as both
    // exist, or the local participant ends up with two pairs: the placeholder
    // they draw into and the one the canonical stream addresses.
    if (drawingEngine && localActorIdRef.current) {
      drawingEngine.setLocalOwner(localActorIdRef.current);
    }
  }, [drawingEngine]);

  // Synchronize temp container ref with real container ref
  useEffect(() => {
    if (canvasContainerRef.current) {
      tempCanvasContainerRef.current = canvasContainerRef.current;
    }
  }, [canvasContainerRef]);

  /**
   * Open at a size that is actually on screen.
   *
   * The toolboxes float rather than dock, so nothing in the layout keeps the
   * canvas out from under them, and the canvas is centred in the whole
   * workspace whatever else is over it. On a phone that is the difference
   * between a drawing and a drawing with two panels sitting on it: 390px of
   * viewport against a 300px canvas leaves 45px a side, and a panel needs 68.
   *
   * So the space the panels will take is reserved before the canvas is fitted
   * into what is left. Never above 100%: a small canvas on a desktop opens at
   * its own size, the way it always has, and this does nothing at all.
   *
   * It sits below the ref synchronisation above deliberately -- that is what
   * hands the zoom controls their canvas, and a fit with no canvas to measure
   * is a silent no-op.
   */
  const fittedOnOpen = useRef(false);
  useEffect(() => {
    if (fittedOnOpen.current || controls.kind === "none") return;
    const workspace = appRef.current;
    if (!workspace || !tempCanvasContainerRef.current) return;
    if (workspace.clientWidth === 0) return;
    fittedOnOpen.current = true;
    const narrow = workspace.clientWidth < NARROW_WORKSPACE;
    zoomToFit({
      // The two-tone toolbox is one panel and sizes itself, so it gets no
      // reservation; the pair of NEO panels take an edge each.
      reservedWidth: narrow && !twoToneConfig ? TOOLBOX_LANE * 2 : 0,
      maximumZoom: 1,
    });
    setOpenedAtFittedZoom(true);
  }, [controls.kind, twoToneConfig, zoomToFit]);

  // Ensure drawing engine DOM canvases are updated when engine becomes available
  useEffect(() => {
    if (drawingEngine) {
      // Update the DOM canvas update function
      domCanvasUpdateRef.current = () => {
        drawingEngine.updateAllDOMCanvasesImmediate();
      };

      // Force an immediate update of all DOM canvases
      setTimeout(() => {
        drawingEngine.updateAllDOMCanvasesImmediate();
      }, 0);
    }
  }, [drawingEngine]);


  // Two-tone canvases open on their fixed background. Continuation images are
  // loaded explicitly by the host through PainterHandle.loadImage().
  useEffect(() => {
    if (drawingEngine && twoToneConfig) {
      initializeTwoToneCanvas(twoToneConfig.backgroundColor);
    }
  }, [
    drawingEngine,
    twoToneConfig,
    initializeTwoToneCanvas,
  ]);

  // Drawing alarm, offered in two-tone mode
  const handleTimerExpire = useCallback(() => {
    // The site's own alert (confirm_dialog.jinja), which every page the
    // painter is served in has; never the browser's (frontend/shared/siteDialog.ts).
    const site = (window as unknown as { dsAlert?: (message: string) => void }).dsAlert;
    if (site) site(t`Time's up.`);
    else console.warn(t`Time's up.`);
  }, [t]);

  const {
    durationMinutes: timerMinutes,
    remainingSeconds: timerRemainingSeconds,
    startTimer,
    stopTimer,
  } = useDrawingTimer({ onExpire: handleTimerExpire });

  const handleTimerChange = useCallback(
    (minutes: number) => {
      if (minutes === 0) {
        stopTimer();
      } else {
        startTimer(minutes);
      }
    },
    [startTimer, stopTimer]
  );

  const [showShortcuts, setShowShortcuts] = useState(false);

  /**
   * The full painter's shortcuts. Two-tone mode keeps Tegaki's pen semantics
   * instead, so the two are mutually exclusive rather than layered.
   */
  const handleShortcut = useCallback(
    (action: ShortcutAction) => {
      switch (action.kind) {
        case "tool":
          updateBrushType(action.tool);
          break;
        case "drawType":
          setDrawingState((prev) => ({ ...prev, drawType: action.drawType }));
          break;
        case "size":
          setDrawingState((prev) => ({
            ...prev,
            brushSize: Math.min(
              MAX_BRUSH_SIZE,
              Math.max(MIN_BRUSH_SIZE, prev.brushSize + action.delta)
            ),
          }));
          break;
        case "zoom":
          if (action.delta > 0) handleZoomIn();
          else handleZoomOut();
          break;
        case "toggleLayer":
          setDrawingState((prev) => ({
            ...prev,
            layerType:
              prev.layerType === "foreground" ? "background" : "foreground",
          }));
          break;
        case "undo":
          if (historyState.canUndo) undo();
          break;
        case "redo":
          if (historyState.canRedo) redo();
          break;
        case "help":
          setShowShortcuts((open) => !open);
          break;
      }
    },
    [
      updateBrushType, setDrawingState, handleZoomIn, handleZoomOut,
      historyState.canUndo, historyState.canRedo, undo, redo,
    ]
  );

  usePainterShortcuts({
    enabled: twoToneConfig === null,
    onAction: handleShortcut,
  });


  // Tegaki-style pen shortcuts, two-tone mode only
  useTwoToneShortcuts({
    enabled: twoToneConfig !== null,
    onSelectPen: selectPen,
    onSwapPen: swapPen,
    onAdjustPenSize: adjustPenSize,
  });

  // Add keyboard shortcuts for undo/redo
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey) {
        if (e.key === "z" && !e.shiftKey) {
          e.preventDefault();
          if (historyState.canUndo) {
            undo();
          }
        } else if (e.key === "y" || (e.key === "z" && e.shiftKey)) {
          e.preventDefault();
          if (historyState.canRedo) {
            redo();
          }
        }
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [undo, redo, historyState.canUndo, historyState.canRedo]);

  return (
    <div className="w-full painter-root flex flex-col">
      <div className="flex-1 flex overflow-hidden">
        <PainterWorkspace
          workspaceRef={appRef}
          canvas={{
            width: canvasWidth,
            height: canvasHeight,
            zoom: currentZoom,
            brushType: drawingState.brushType,
            brushSize: drawingState.brushSize,
            color: drawingState.color,
            containerRef: canvasContainerRef,
            interactionRef: tempLocalUserCanvasRef,
            cursorRef: cursorCanvasRef,
            previewRef: previewCanvasRef,
            textBoxRef,
            textAt,
            onTextKeyDown: handleTextKey,
            onDismissText: () => setTextAt(null),
          }}
        >
            {controls.kind === "toolbox" && <ShortcutHelp
              open={showShortcuts}
              onClose={() => setShowShortcuts(false)}
            />}
            {controls.kind === "toolbox" && twoToneConfig === null && (
              // A shortcut nobody can find is a shortcut nobody uses, so the
              // key that opens the list is also a button.
              <button
                type="button"
                onClick={() => setShowShortcuts(true)}
                title="Keyboard shortcuts (?)"
                aria-label="Keyboard shortcuts"
                // `touch-none` for the same reason the panels have it: this
                // acts on `click`, and a stylus press that slides is taken for
                // a scroll and never produces one.
                className={`${NEO_BUTTON} fixed bottom-3 right-3 z-40 h-7 w-7 touch-none text-sm`}
              >
                ?
              </button>
            )}
            {controls.kind === "none" || !openedAtFittedZoom ? null : twoToneConfig ? (
              <SimplifiedToolbox
                anchorRef={appRef}
                canvasRef={canvasContainerRef}
                brushSize={drawingState.brushSize}
                paletteColors={paletteColors}
                selectedPaletteIndex={selectedPaletteIndex}
                canUndo={historyState.canUndo}
                canRedo={historyState.canRedo}
                timerMinutes={timerMinutes}
                timerRemainingSeconds={timerRemainingSeconds}
                onBrushSizeChange={setPenSize}
                onSelectPen={selectPen}
                onTimerChange={handleTimerChange}
                onUndo={undo}
                onRedo={redo}
              />
            ) : (
              <ToolboxPanels
                // The painter's own area, which now really is where the
                // painter is: its root fills the element the host mounted it
                // into rather than pinning itself to the viewport.
                anchorRef={appRef}
                canvasRef={canvasContainerRef}
                // An offline drawing records lineType straight into the .pch,
                // which already has codes for every one of these.
                tools={ALL_TOOLS}
                drawingState={drawingState}
                historyState={historyState}
                paletteColors={paletteColors}
                selectedPaletteIndex={selectedPaletteIndex}
                currentZoom={currentZoom}
                isOwner={false}
                isSaving={false}
                sessionEnded={false}
                onUndo={undo}
                onRedo={redo}
                onUpdateBrushType={updateBrushType}
                onUpdateDrawingState={setDrawingState}
                onUpdateColor={updateColor}
                onSetSelectedPaletteIndex={setSelectedPaletteIndex}
                onSetPaletteColor={setPaletteColor}
                palettePresets={palettePresets}
                onApplyPalette={setPaletteColors}
                onZoomIn={() => handleZoomIn()}
                onZoomOut={() => handleZoomOut()}
                onZoomReset={handleZoomReset}
                onZoomFit={handleZoomFit}
                onSaveCollaborativeDrawing={() => {}}
                virtualRight={virtualRight}
                onToggleVirtualRight={toggleVirtualRight}
                // Only worth a panel once there is more than one participant;
                // a solitary painter has nobody to hide and nowhere else to
                // draw, and NEO's own layer button already covers their pair.
                participantLayers={participantLayers.length > 1 ? participantLayers : undefined}
                hiddenOwners={hiddenOwners}
                targetOwner={targetOwner ?? localActorIdRef.current}
                localActorId={localActorIdRef.current}
                onToggleOwnerVisible={toggleOwnerVisible}
                onSelectTargetOwner={selectTargetOwner}
                layersOrigin={layersOrigin ?? undefined}
                drawThumbnail={drawThumbnail}
                canvasAspect={canvasWidth / canvasHeight}
              />
            )}
        </PainterWorkspace>
      </div>
    </div>
  );
});

export default Painter;
