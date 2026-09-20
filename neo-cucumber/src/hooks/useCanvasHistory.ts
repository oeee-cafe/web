import { useRef, useCallback } from "react";
import {
  restoreLayer,
  retainedBytes,
  snapshotLayer,
  type LayerSnapshot,
} from "../utils/layerSnapshots";

/**
 * One undoable state of both layers.
 *
 * The layers are tiled snapshots rather than copies: consecutive entries share
 * the buffer for every tile the stroke between them did not touch, so a thirty
 * deep stack costs a few strokes' worth of tiles instead of sixty canvases.
 * The tiles are immutable and shared -- read them through `restoreInto`.
 */
export interface CanvasState {
  foreground: LayerSnapshot;
  background: LayerSnapshot;
  timestamp: number;
  /** True if this snapshot contains content received from collaboration. */
  isContentSnapshot?: boolean;
}

export const useCanvasHistory = (maxHistorySize: number = 30) => {
  const historyRef = useRef<CanvasState[]>([]);
  const currentIndexRef = useRef(-1);
  const hasDrawingActionsRef = useRef(false);

  const saveState = useCallback(
    (
      foreground: Uint8ClampedArray,
      background: Uint8ClampedArray,
      width: number,
      height: number,
      isDrawingAction: boolean = true,
      isContentSnapshot: boolean = false
    ) => {
      // Every stroke gets an entry, even one that changed no pixels. Canonical
      // Neo registers an undo step unconditionally at stroke start (tools.js
      // freeHandDownHandler), and the action recorder likewise emits one frame
      // per stroke. Skipping "duplicate" states here desynchronised the two: the
      // recorder's head advanced while the history index did not, so undoing
      // back past an inert stroke left the replay one stroke ahead of the canvas
      // and of the saved PNG.

      // Shared against the entry this one follows, which is the entry at the
      // current index -- not the last in the list. After an undo those differ,
      // and the tail is about to be discarded anyway.
      const previous = historyRef.current[currentIndexRef.current] ?? null;
      const newState: CanvasState = {
        foreground: snapshotLayer(
          foreground,
          width,
          height,
          previous?.foreground ?? null
        ),
        background: snapshotLayer(
          background,
          width,
          height,
          previous?.background ?? null
        ),
        timestamp: Date.now(),
        isContentSnapshot,
      };

      // Remove any states after current index (when user made new changes after undo)
      if (currentIndexRef.current < historyRef.current.length - 1) {
        historyRef.current = historyRef.current.slice(
          0,
          currentIndexRef.current + 1
        );
      }

      historyRef.current.push(newState);

      // Track if this is a drawing action
      if (isDrawingAction) {
        hasDrawingActionsRef.current = true;
      }

      // Limit history size
      if (historyRef.current.length > maxHistorySize) {
        historyRef.current = historyRef.current.slice(-maxHistorySize);
        currentIndexRef.current = maxHistorySize - 1;
      } else {
        currentIndexRef.current = historyRef.current.length - 1;
      }
    },
    [maxHistorySize]
  );

  /** Writes a state's layers back over the live buffers. */
  const restoreInto = useCallback(
    (
      state: CanvasState,
      foreground: Uint8ClampedArray,
      background: Uint8ClampedArray
    ) => {
      restoreLayer(state.foreground, foreground);
      restoreLayer(state.background, background);
    },
    []
  );

  const undo = useCallback((): CanvasState | null => {
    if (currentIndexRef.current > 0) {
      currentIndexRef.current--;
      return historyRef.current[currentIndexRef.current];
    }
    return null;
  }, []);

  const redo = useCallback((): CanvasState | null => {
    if (currentIndexRef.current < historyRef.current.length - 1) {
      currentIndexRef.current++;
      return historyRef.current[currentIndexRef.current];
    }
    return null;
  }, []);

  const canUndo = useCallback((): boolean => {
    // Can't undo if we don't have drawing actions or are at the beginning
    if (!hasDrawingActionsRef.current || currentIndexRef.current <= 0) {
      return false;
    }

    // Find the last content snapshot (BG/FG layers received from collaboration)
    let lastContentSnapshotIndex = -1;
    for (let i = historyRef.current.length - 1; i >= 0; i--) {
      if (historyRef.current[i].isContentSnapshot) {
        lastContentSnapshotIndex = i;
        break;
      }
    }

    // If there's a content snapshot, prevent undoing past it
    if (lastContentSnapshotIndex !== -1) {
      return currentIndexRef.current > lastContentSnapshotIndex;
    }

    // Otherwise, can undo as long as we're not at the beginning
    return currentIndexRef.current > 0;
  }, []);

  const canRedo = useCallback((): boolean => {
    return currentIndexRef.current < historyRef.current.length - 1;
  }, []);

  const getHistoryInfo = useCallback(() => {
    return {
      currentIndex: currentIndexRef.current,
      historyLength: historyRef.current.length,
      canUndo: canUndo(),
      canRedo: canRedo(),
      /** What the stack actually holds, counting a shared tile once. */
      // Two passes rather than `flatMap`, which is Firefox 62 and this
      // package targets 56. `retainedBytes` counts each tile once by
      // identity, so it does not care that the layers arrive regrouped.
      retainedBytes: retainedBytes(
        historyRef.current
          .map((state) => state.foreground)
          .concat(historyRef.current.map((state) => state.background))
      ),
    };
  }, [canUndo, canRedo]);

  const clearHistory = useCallback(() => {
    historyRef.current = [];
    currentIndexRef.current = -1;
    hasDrawingActionsRef.current = false;
  }, []);

  return {
    saveState,
    restoreInto,
    undo,
    redo,
    canUndo,
    canRedo,
    getHistoryInfo,
    clearHistory,
  };
};
