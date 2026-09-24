import { act, useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { usePainterDrawing } from "../hooks/usePainterDrawing";
import type { PasteDisplay } from "../neo/regionPreview";
import type { DrawingState } from "../types/drawing";
import type { ToolId } from "../neo/tools";
import type { RegionRect } from "../neo/regionDrag";
import {
  LAYER,
  createCanonicalPainter,
  decodePCH,
  readPixels,
  replayWithNeo,
} from "./neoHarness";

/**
 * The real offline drawing stack under real pointer events, with a tool
 * state that follows the painter's own tool changes -- as the toolbox does
 * -- so a copy that hands over to paste actually lands in paste.
 */
export const W = 60;
export const H = 40;
export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export async function mountPainter(initialTool: ToolId) {
  const tools: ToolId[] = [];
  const previews: (PasteDisplay | null)[] = [];
  const regionPreviews: (RegionRect | null)[] = [];
  type Layer = DrawingState["layerType"];
  const handle: {
    api: ReturnType<typeof usePainterDrawing> | null;
    setTool: (tool: ToolId) => void;
    setLayer: (layer: Layer) => void;
    tool: ToolId;
  } = { api: null, setTool: () => {}, setLayer: () => {}, tool: initialTool };

  function Harness() {
    const appRef = useRef<HTMLDivElement>(null);
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const [brushType, setBrushType] = useState<ToolId>(initialTool);
    const [layerType, setLayerType] = useState<Layer>("background");
    const state: DrawingState = {
      brushSize: 1,
      opacity: 255,
      color: "#1e2864",
      brushType,
      layerType,
      zoomLevel: 100,
      fgVisible: true,
      bgVisible: true,
      isFlippedHorizontal: false,
    };
    const placement = useMemo(
      () => ({
        onToolChange: (tool: ToolId) => {
          tools.push(tool);
          setBrushType(tool);
        },
        onPastePreview: (p: PasteDisplay | null) => previews.push(p),
      }),
      []
    );
    const api = usePainterDrawing({
      canvasRef, appRef, drawingState: state,
      zoomLevel: 100, canvasWidth: W, canvasHeight: H,
      previews: { onRegionPreview: (rect: RegionRect | null) => regionPreviews.push(rect) },
      placement,
      mode: { kind: "offline" },
    });
    useEffect(() => {
      handle.api = api;
      handle.setTool = setBrushType;
      handle.setLayer = setLayerType;
      handle.tool = brushType;
    }, [api, brushType]);
    return (
      <div ref={appRef}>
        <canvas id="canvas" ref={canvasRef} width={W} height={H}
          style={{ width: `${W}px`, height: `${H}px`, display: "block" }} />
      </div>
    );
  }

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(async () => { root.render(<Harness />); });
  for (let i = 0; i < 50 && !handle.api?.drawingEngine; i++) {
    await act(async () => { await sleep(10); root.render(<Harness />); });
  }
  if (!handle.api?.drawingEngine) throw new Error("engine never initialised");

  const canvas = container.querySelector("#canvas") as HTMLCanvasElement;
  const box = canvas.getBoundingClientRect();
  const send = async (type: string, x: number, y: number) => {
    await act(async () => {
      canvas.dispatchEvent(new PointerEvent(type, {
        pointerId: 1, pointerType: "mouse", button: 0,
        buttons: type === "pointerup" ? 0 : 1,
        clientX: box.left + x, clientY: box.top + y,
        bubbles: true, cancelable: true,
      }));
    });
  };
  const drag = async (x0: number, y0: number, x1: number, y1: number) => {
    await send("pointerdown", x0, y0);
    await act(async () => { await sleep(20); });
    await send("pointermove", x1, y1);
    await send("pointerup", x1, y1);
  };
  const selectTool = async (tool: ToolId) => {
    await act(async () => { handle.setTool(tool); });
  };
  const selectLayer = async (layer: Layer) => {
    await act(async () => { handle.setLayer(layer); });
  };
  const layer = () => handle.api!.drawingEngine!.layers.background;
  const alphaAt = (x: number, y: number) => layer()[(y * W + x) * 4 + 3];
  const frames = async () =>
    (await decodePCH(handle.api!.replay!.getReplayBlob())).items;

  return {
    handle, tools, previews, regionPreviews, send, drag,
    selectTool, selectLayer, layer, alphaAt, frames,
  };
}

/** The recording, re-rendered by NEO itself, background layer. */
export async function neoRendering(blob: Blob) {
  const decoded = await decodePCH(blob);
  const cp = createCanonicalPainter(W, H);
  replayWithNeo(cp, decoded.items);
  return readPixels(cp.contexts[LAYER.BACKGROUND], W, H);
}

