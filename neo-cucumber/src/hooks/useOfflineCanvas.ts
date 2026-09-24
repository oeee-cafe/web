import { useRef, useEffect, useCallback } from "react";
import { DrawingEngine } from "../DrawingEngine";
import {
  compositeLayersToCanvas,
  downloadCanvasAsPNG as downloadCanvas,
} from "../utils/canvasExport";
import { bottomFirst, inJoinOrder, participantZIndex } from "../neo/canvasStack";

interface UseOfflineCanvasParams {
  canvasWidth: number;
  canvasHeight: number;
  drawingEngine: DrawingEngine | null;
  /**
   * Participants whose layers are hidden from this viewer.
   *
   * A local view setting: hiding somebody is a way of seeing your own work,
   * not an edit, so it never travels and never reaches the exported image.
   */
  hiddenOwners?: ReadonlySet<string>;
  /**
   * NEO's own layer visibility, which is a view over both tiers rather than
   * over a participant: its layer button hides the background or the
   * foreground, and with a pair per participant that means everybody's.
   */
  bgVisible?: boolean;
  fgVisible?: boolean;
}

export const useOfflineCanvas = ({
  canvasWidth,
  canvasHeight,
  drawingEngine,
  hiddenOwners,
  bgVisible = true,
  fgVisible = true,
}: UseOfflineCanvasParams) => {
  const canvasContainerRef = useRef<HTMLDivElement>(null);
  /** One background/foreground canvas pair per participant. */
  const ownerCanvasesRef = useRef(
    new Map<string, { background: HTMLCanvasElement; foreground: HTMLCanvasElement }>(),
  );
  const hiddenRef = useRef<ReadonlySet<string>>(hiddenOwners ?? new Set());
  const tiersRef = useRef({ background: bgVisible, foreground: fgVisible });

  /**
   * Whether a canvas is shown, from both of the things that can hide it: the
   * participant it belongs to, and the tier it is in. `display` carries the
   * answer, so anything reading the screen -- the colour picker especially --
   * gets the same one without being told about either.
   */
  const shownFor = (owner: string, layer: "background" | "foreground") =>
    !hiddenRef.current.has(owner) && tiersRef.current[layer];

  /**
   * Gives every participant a canvas pair and puts the stack in join order.
   *
   * Called again whenever the engine gains or loses a participant, which
   * happens the first time an operation names one -- a late joiner's first
   * stroke brings their layers into being.
   */
  const syncOwnerCanvases = useCallback(() => {
    const container = canvasContainerRef.current;
    if (!container || !drawingEngine) return;
    const canvasContent =
      container.querySelector<HTMLElement>(".canvas-content") ?? container;

    const owners = inJoinOrder(drawingEngine.ownerIds());
    const live = new Set(owners);
    for (const [owner, pair] of ownerCanvasesRef.current) {
      if (live.has(owner)) continue;
      pair.background.remove();
      pair.foreground.remove();
      ownerCanvasesRef.current.delete(owner);
    }

    owners.forEach((owner, rank) => {
      let pair = ownerCanvasesRef.current.get(owner);
      if (!pair) {
        const make = () => {
          const canvas = document.createElement("canvas");
          canvas.width = canvasWidth;
          canvas.height = canvasHeight;
          canvas.className = "absolute top-0 left-0 pointer-events-none";
          canvas.style.width = `${canvasWidth}px`;
          canvas.style.height = `${canvasHeight}px`;
          canvasContent.appendChild(canvas);
          return canvas;
        };
        pair = { background: make(), foreground: make() };
        ownerCanvasesRef.current.set(owner, pair);
        drawingEngine.attachDOMCanvases(pair.background, pair.foreground, owner);
      }
      pair.background.style.zIndex = String(participantZIndex(rank, "background"));
      pair.foreground.style.zIndex = String(participantZIndex(rank, "foreground"));
      pair.background.style.display = shownFor(owner, "background") ? "" : "none";
      pair.foreground.style.display = shownFor(owner, "foreground") ? "" : "none";
    });

    drawingEngine.updateAllDOMCanvasesImmediate();
  }, [canvasWidth, canvasHeight, drawingEngine]);

  useEffect(() => {
    if (!drawingEngine) return;
    syncOwnerCanvases();
    return drawingEngine.onOwnersChanged(syncOwnerCanvases);
  }, [drawingEngine, syncOwnerCanvases]);

  // Show and hide participants and tiers without disturbing the stack
  useEffect(() => {
    hiddenRef.current = hiddenOwners ?? new Set();
    tiersRef.current = { background: bgVisible, foreground: fgVisible };
    for (const [owner, pair] of ownerCanvasesRef.current) {
      pair.background.style.display = shownFor(owner, "background") ? "" : "none";
      pair.foreground.style.display = shownFor(owner, "foreground") ? "" : "none";
    }
    // `shownFor` reads the refs this effect has just set.
  }, [hiddenOwners, bgVisible, fgVisible]);

  /**
   * Flattens the whole stack, everyone included.
   *
   * Hiding a participant is a way of looking at the drawing, so the exported
   * image carries every participant's work whether or not this viewer had
   * them showing.
   */
  const compositeCanvasesForExport =
    useCallback((): HTMLCanvasElement | null => {
      if (!drawingEngine) return null;

      const layers: HTMLCanvasElement[] = [];
      for (const owner of bottomFirst(drawingEngine.ownerIds())) {
        for (const layer of ["background", "foreground"] as const) {
          const canvas = drawingEngine.getLayerCanvas(layer, owner);
          if (canvas) layers.push(canvas);
        }
      }

      return compositeLayersToCanvas(canvasWidth, canvasHeight, layers);
    }, [drawingEngine, canvasWidth, canvasHeight]);

  // Download canvas as PNG
  const downloadCanvasAsPNG = useCallback(() => {
    const exportCanvas = compositeCanvasesForExport();
    if (!exportCanvas) {
      console.error("Failed to create export canvas");
      return;
    }

    downloadCanvas(exportCanvas);
  }, [compositeCanvasesForExport]);

  return {
    canvasContainerRef,
    compositeCanvasesForExport,
    downloadCanvasAsPNG,
  };
};
