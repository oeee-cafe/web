import { useCallback, useRef, useEffect } from "react";
import { DrawingEngine } from "../DrawingEngine";
import {
  compositeLayersToCanvas,
  downloadCanvasAsPNG as downloadCanvas,
} from "../utils/canvasExport";
import { CANVAS_Z_INDEX } from "../neo/canvasStack";

interface CanvasHookParams {
  canvasMeta: { width: number; height: number } | null;
  drawingEngine: DrawingEngine | null;
  currentZoom: number;
  drawingState: {
    fgVisible: boolean;
    bgVisible: boolean;
  };
}

// All participants draw on one shared background/foreground layer pair
// rendered by the single shared drawing engine.
export const useCanvas = ({
  canvasMeta,
  drawingEngine,
  currentZoom,
  drawingState,
}: CanvasHookParams) => {
  const canvasContainerRef = useRef<HTMLDivElement>(null);
  const bgCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const fgCanvasRef = useRef<HTMLCanvasElement | null>(null);

  // Creates the shared layer canvases and attaches them to the engine
  useEffect(() => {
    if (
      !drawingEngine ||
      !canvasContainerRef.current ||
      !canvasMeta?.width ||
      !canvasMeta?.height
    ) {
      return;
    }

    const container = canvasContainerRef.current;
    const canvasContent =
      container.querySelector<HTMLElement>(".canvas-content") ?? container;
    const interactionCanvas = canvasContent.querySelector("#canvas");

    const createLayerCanvas = (id: string, zIndex: number) => {
      const canvas = document.createElement("canvas");
      canvas.width = canvasMeta.width;
      canvas.height = canvasMeta.height;
      canvas.className = "absolute top-0 left-0 canvas-bg";
      canvas.style.pointerEvents = "none"; // Events handled by interaction canvas
      canvas.style.width = `${canvasMeta.width}px`;
      canvas.style.height = `${canvasMeta.height}px`;
      canvas.style.zIndex = zIndex.toString();
      canvas.id = id;

      const ctx = canvas.getContext("2d");
      if (ctx) {
        ctx.imageSmoothingEnabled = false;
      }

      if (interactionCanvas) {
        canvasContent.insertBefore(canvas, interactionCanvas);
      } else {
        canvasContent.appendChild(canvas);
      }
      return canvas;
    };

    if (!bgCanvasRef.current) {
      bgCanvasRef.current = createLayerCanvas(
        "bg-shared",
        CANVAS_Z_INDEX.background
      );
    }
    if (!fgCanvasRef.current) {
      fgCanvasRef.current = createLayerCanvas(
        "fg-shared",
        CANVAS_Z_INDEX.foreground
      );
    }

    drawingEngine.attachDOMCanvases(bgCanvasRef.current, fgCanvasRef.current);
    drawingEngine.updateAllDOMCanvasesImmediate();
  }, [drawingEngine, canvasMeta?.width, canvasMeta?.height]);

  // Update canvas when layer visibility changes
  useEffect(() => {
    if (bgCanvasRef.current) {
      bgCanvasRef.current.style.display = drawingState.bgVisible
        ? "block"
        : "none";
    }
    if (fgCanvasRef.current) {
      fgCanvasRef.current.style.display = drawingState.fgVisible
        ? "block"
        : "none";
    }
  }, [drawingState.fgVisible, drawingState.bgVisible]);

  // Function to composite the shared layers for export
  const compositeCanvasesForExport = useCallback(() => {
    if (!canvasMeta?.width || !canvasMeta?.height) return null;

    const layers = [bgCanvasRef.current, fgCanvasRef.current].filter(
      (c): c is HTMLCanvasElement => c !== null
    );
    return compositeLayersToCanvas(canvasMeta.width, canvasMeta.height, layers);
  }, [canvasMeta]);

  // Function to download current canvas as PNG
  const downloadCanvasAsPNG = useCallback(() => {
    if (!canvasMeta?.width || !canvasMeta?.height) return;

    try {
      const tempCanvas = compositeCanvasesForExport();
      if (!tempCanvas) return;

      downloadCanvas(tempCanvas);
    } catch (error) {
      console.error("Error in downloadCanvasAsPNG:", error);
    }
  }, [compositeCanvasesForExport, canvasMeta?.width, canvasMeta?.height]);

  // Update canvas transform when zoom changes
  useEffect(() => {
    if (drawingEngine && canvasContainerRef.current) {
      drawingEngine.updatePanOffset(
        0,
        0,
        canvasContainerRef.current,
        currentZoom
      );
    }
  }, [currentZoom, drawingEngine]);

  return {
    canvasContainerRef,
    compositeCanvasesForExport,
    downloadCanvasAsPNG,
  };
};
