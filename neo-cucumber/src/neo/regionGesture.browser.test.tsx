import { describe, expect, it } from "vitest";
import { act, useEffect, useRef } from "react";
import { createRoot } from "react-dom/client";
import { useOfflineDrawing } from "../hooks/useOfflineDrawing";
import type { DrawingState } from "../types/drawing";
import type { PainterOperation } from "../operations";
import { drawRegionPreview } from "./regionPreview";
import type { RegionRect } from "./regionDrag";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const W = 60;
const H = 40;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** The last frame of the recording, as NEO would read it. */
async function lastFrame(api: { getReplayBlob: () => Blob }) {
  const bytes = new Uint8Array(await api.getReplayBlob().arrayBuffer());
  const { decodePCH } = await import("./NeoReplay");
  return decodePCH(bytes)!.items.at(-1)!;
}

/** Mounts the real offline stack with a region tool selected. */
async function mountWithTool(
  tool: string,
  extra: Partial<DrawingState> = {},
  onOperation?: (operation: PainterOperation) => void,
) {
  const previews: (RegionRect | null)[] = [];
  const captured: { api: ReturnType<typeof useOfflineDrawing> | null } = { api: null };

  const state: DrawingState = {
    brushSize: 4, opacity: 255, color: "#1e2864",
    brushType: tool as DrawingState["brushType"],
    layerType: "background", zoomLevel: 100,
    fgVisible: true, bgVisible: true, isFlippedHorizontal: false,
    ...extra,
  };

  function Harness() {
    const appRef = useRef<HTMLDivElement>(null);
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const api = useOfflineDrawing(canvasRef, appRef, state, undefined, 100, W, H,
      undefined, undefined, (r: RegionRect | null) => previews.push(r),
      undefined, undefined, undefined, undefined, false, onOperation);
    useEffect(() => { captured.api = api; });
    return (
      <div id="app" ref={appRef}>
        <canvas id="canvas" ref={canvasRef} width={W} height={H}
          style={{ width: `${W}px`, height: `${H}px`, display: "block" }} />
      </div>
    );
  }

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(async () => { root.render(<Harness />); });
  for (let i = 0; i < 50 && !captured.api?.drawingEngine; i++) {
    await act(async () => { await sleep(10); root.render(<Harness />); });
  }
  if (!captured.api?.drawingEngine) throw new Error("engine never initialised");

  const canvas = container.querySelector("#canvas") as HTMLCanvasElement;
  const rect = canvas.getBoundingClientRect();
  const send = async (type: string, x: number, y: number) => {
    await act(async () => {
      canvas.dispatchEvent(new PointerEvent(type, {
        pointerId: 1, pointerType: "mouse", button: 0,
        buttons: type === "pointerup" ? 0 : 1,
        clientX: rect.left + x, clientY: rect.top + y,
        bubbles: true, cancelable: true,
      }));
    });
  };

  return { api: captured.api, previews, send, canvas };
}

describe("dragging out a region tool", () => {
  it("previews while dragging and applies on release", async () => {
    const { api, previews, send } = await mountWithTool("rectFill");
    const layer = api.drawingEngine!.layers.background;
    expect(layer[(20 * W + 20) * 4 + 3]).toBe(0);

    await send("pointerdown", 10, 8);
    await act(async () => { await sleep(20); });
    await send("pointermove", 34, 26);
    await act(async () => { await sleep(20); });
    await send("pointerup", 34, 26);

    // Nothing on the press -- EffectToolBase.downHandler draws no cursor --
    // then the rectangle once it moves, cleared on release
    expect(previews.length).toBeGreaterThan(1);
    expect(previews[0]).toEqual({ x: 10, y: 8, width: 25, height: 19 });
    expect(previews.at(-1)).toBeNull();

    // A pixel inside the dragged rectangle is now painted
    expect(layer[(20 * W + 20) * 4 + 3]).toBeGreaterThan(0);
    // and one outside it is not
    expect(layer[(2 * W + 50) * 4 + 3]).toBe(0);
  });

  it("records the region as its own replay frame", async () => {
    const { api, send } = await mountWithTool("eraseRect");
    await send("pointerdown", 6, 6);
    await act(async () => { await sleep(20); });
    await send("pointermove", 20, 18);
    await send("pointerup", 20, 18);

    const bytes = new Uint8Array(await api.getReplayBlob().arrayBuffer());
    const { decodePCH } = await import("./NeoReplay");
    const decoded = decodePCH(bytes)!;
    const frame = decoded.items.at(-1)!;
    expect(frame[0]).toBe("eraseRect2");
    // pushCurrent occupies 2..10, so geometry starts at 11
    expect(frame.slice(11, 15)).toEqual([6, 6, 15, 13]);
  });

  it("records the mask the region was drawn through", async () => {
    // The mask only ever reached the recorder from a freehand press. A region
    // dragged out with the mask on and no freehand stroke before it -- or
    // after a stroke made with the mask off -- was masked on this canvas and
    // recorded as unmasked, so NEO and the room drew it over everything.
    const { api, send } = await mountWithTool("rectFill", {
      maskType: 1, maskColor: "#ff0000",
    });
    await send("pointerdown", 6, 6);
    await act(async () => { await sleep(20); });
    await send("pointermove", 20, 18);
    await send("pointerup", 20, 18);

    const frame = await lastFrame(api);
    expect(frame[0]).toBe("fill");
    // pushCurrent: [verb, layer, r, g, b, a, mask r, g, b, size, mask type]
    expect(frame.slice(6, 9)).toEqual([255, 0, 0]);
    expect(frame[10]).toBe(1);
  });

  it("leaves the canvas alone when the drag is cancelled", async () => {
    const { api, previews, send, canvas } = await mountWithTool("rectFill");
    const layer = api.drawingEngine!.layers.background;

    await send("pointerdown", 10, 8);
    await act(async () => { await sleep(20); });
    await send("pointermove", 30, 24);
    await act(async () => {
      // This harness's canvas, not whichever one another test left in the DOM
      canvas.dispatchEvent(
        new PointerEvent("pointercancel", { pointerId: 1, bubbles: true })
      );
    });

    expect(previews.at(-1)).toBeNull();
    expect(layer.some((v) => v !== 0)).toBe(false);
  });
});

describe("the preview overlay", () => {
  it("draws an outline that survives whatever is underneath", () => {
    const artwork = document.createElement("canvas");
    artwork.width = 40; artwork.height = 20;
    const art = artwork.getContext("2d", { willReadFrequently: true })!;

    // Half white, half black: a fixed colour would vanish on one of them
    art.fillStyle = "#ffffff"; art.fillRect(0, 0, 20, 20);
    art.fillStyle = "#000000"; art.fillRect(20, 0, 20, 20);
    const before = new Uint8ClampedArray(art.getImageData(0, 0, 40, 20).data);

    const canvas = document.createElement("canvas");
    canvas.width = 40; canvas.height = 20;
    const ctx = canvas.getContext("2d", { willReadFrequently: true })!;

    drawRegionPreview(
      ctx,
      { x: 2, y: 2, width: 36, height: 16 },
      { width: 40, height: 20, layers: [before] },
      "rect"
    );
    art.drawImage(canvas, 0, 0);
    const after = art.getImageData(0, 0, 40, 20).data;

    let changedOnWhite = 0, changedOnBlack = 0;
    for (let i = 0; i < before.length; i += 4) {
      if (before[i] === after[i] && before[i + 1] === after[i + 1]) continue;
      const x = (i / 4) % 40;
      if (x < 20) changedOnWhite++; else changedOnBlack++;
    }
    expect(changedOnWhite).toBeGreaterThan(0);
    expect(changedOnBlack).toBeGreaterThan(0);
  });

  it("clears when there is no rectangle", () => {
    const canvas = document.createElement("canvas");
    canvas.width = 20; canvas.height = 20;
    const ctx = canvas.getContext("2d", { willReadFrequently: true })!;
    const backdrop = {
      width: 20,
      height: 20,
      layers: [new Uint8ClampedArray(20 * 20 * 4)],
    };
    drawRegionPreview(ctx, { x: 2, y: 2, width: 10, height: 10 }, backdrop);
    drawRegionPreview(ctx, null, backdrop);
    const px = ctx.getImageData(0, 0, 20, 20).data;
    expect(px.some((v) => v !== 0)).toBe(false);
  });
});

describe("eraseAll, which acts on the press itself", () => {
  it("clears the pair the painter is aimed at", async () => {
    // The engine's interactive draws default to the draw target now, so a
    // participant clearing somebody else's layer clears it on their own
    // screen too -- not their own layer, while the room cleared the other.
    const { api, send } = await mountWithTool("eraseAll");
    const engine = api.drawingEngine!;
    engine.setDrawTarget("7");
    engine.layersFor("7").background.fill(200);
    engine.layers.background.fill(200);

    await send("pointerdown", 12, 12);
    await send("pointerup", 12, 12);

    expect(engine.layersFor("7").background.every((v) => v === 0)).toBe(true);
    expect(engine.layers.background.every((v) => v === 200)).toBe(true);
  });

  it("clears the layer and records it", async () => {
    // Draw something first, so there is anything to clear
    const drawing = await mountWithTool("rectFill");
    await drawing.send("pointerdown", 6, 6);
    await act(async () => { await sleep(20); });
    await drawing.send("pointermove", 40, 30);
    await drawing.send("pointerup", 40, 30);
    const layer = drawing.api.drawingEngine!.layers.background;
    expect(layer.some((v) => v !== 0)).toBe(true);

    // Selecting eraseAll and clicking wipes it, with no drag involved
    const { api, previews, send } = await mountWithTool("eraseAll");
    const target = api.drawingEngine!.layers.background;
    target.fill(200);

    await send("pointerdown", 12, 12);
    await send("pointerup", 12, 12);

    expect(target.every((v) => v === 0)).toBe(true);
    // No rubber band: it is not a region tool
    expect(previews).toEqual([]);

    const bytes = new Uint8Array(await api.getReplayBlob().arrayBuffer());
    const { decodePCH } = await import("./NeoReplay");
    const frame = decodePCH(bytes)!.items.at(-1)!;
    expect(frame).toEqual(["eraseAll", 0]);
  });
});

describe("the line draw type", () => {
  it("records the mask the line was drawn through", async () => {
    // The mask reached the recorder from the freehand press only; a line
    // never told it, so one drawn with the mask on was recorded unmasked.
    const { api, send } = await mountWithTool("solid", {
      drawType: "line", maskType: 1, maskColor: "#ff0000",
    });
    await send("pointerdown", 8, 8);
    await act(async () => { await sleep(20); });
    await send("pointermove", 40, 30);
    await send("pointerup", 40, 30);

    const frame = await lastFrame(api);
    expect(frame[0]).toBe("line");
    // pushCurrent: [verb, layer, r, g, b, a, mask r, g, b, size, mask type]
    expect(frame.slice(6, 9)).toEqual([255, 0, 0]);
    expect(frame[10]).toBe(1);
  });

  it("draws a straight line on release and records it", async () => {
    const { api, send, previews } = await mountWithTool("solid", { drawType: "line" });
    const layer = api.drawingEngine!.layers.background;

    await send("pointerdown", 8, 8);
    await act(async () => { await sleep(20); });
    await send("pointermove", 40, 30);
    await send("pointerup", 40, 30);

    // Painted along the line, not just at the endpoints
    const at = (x: number, y: number) => layer[(y * W + x) * 4 + 3];
    expect(at(8, 8)).toBeGreaterThan(0);
    expect(at(40, 30)).toBeGreaterThan(0);
    expect(at(24, 19)).toBeGreaterThan(0);
    // and nothing where a freehand path would not have gone either
    expect(at(2, 36)).toBe(0);
    // A straight line is not a region drag, so no rubber band
    expect(previews).toEqual([]);

    const bytes = new Uint8Array(await api.getReplayBlob().arrayBuffer());
    const { decodePCH } = await import("./NeoReplay");
    const frame = decodePCH(bytes)!.items.at(-1)!;
    expect(frame[0]).toBe("line");
    // pushCurrent occupies 2..10; lineType then the endpoints
    expect(frame[11]).toBe(1);
    expect(frame.slice(12, 16)).toEqual([8, 8, 40, 30]);
  });

  it("replays to the same pixels it drew", async () => {
    const { api, send } = await mountWithTool("solid", { drawType: "line" });
    await send("pointerdown", 6, 30);
    await act(async () => { await sleep(20); });
    await send("pointermove", 50, 6);
    await send("pointerup", 50, 6);

    const { decodePCH, NeoReplay } = await import("./NeoReplay");
    const decoded = decodePCH(
      new Uint8Array(await api.getReplayBlob().arrayBuffer())
    )!;
    const replay = new NeoReplay(W, H);
    const buffers = [new Uint8ClampedArray(W * H * 4), new Uint8ClampedArray(W * H * 4)];
    const { BufferSurface } = await import("./PixelSurface");
    replay.painter.surfaces = [
      new BufferSurface(buffers[0], W, H),
      new BufferSurface(buffers[1], W, H),
    ];
    // The replay draws strokes through canvasCtx, so read those instead
    await replay.playAll(decoded.items);

    const ours = api.drawingEngine!.layers.background;
    const theirs = replay.getLayerPixels(0);
    let painted = 0;
    for (let i = 3; i < ours.length; i += 4) {
      if (ours[i] > 0 && theirs[i] > 0) painted++;
    }
    // The same line is there in both
    expect(painted).toBeGreaterThan(30);
  });
});

describe("the bezier draw type", () => {
  /** NEO's gesture: drag out the chord, then click each handle in turn. */
  async function buildCurve(
    send: (t: string, x: number, y: number) => Promise<void>,
    c1: [number, number],
    c2: [number, number]
  ) {
    await send("pointerdown", 8, 30);
    await act(async () => { await sleep(20); });
    await send("pointermove", 50, 30);
    await send("pointerup", 50, 30);          // endpoints set

    await send("pointerdown", ...c1);
    await send("pointerup", ...c1);           // first handle

    await send("pointerdown", ...c2);
    await send("pointerup", ...c2);           // second handle, commits
  }

  it("records the mask the curve was drawn through", async () => {
    const { api, send } = await mountWithTool("solid", {
      drawType: "bezier", maskType: 2, maskColor: "#00ff00",
    });
    await buildCurve(send, [20, 8], [40, 8]);

    const frame = await lastFrame(api);
    expect(frame[0]).toBe("bezier");
    expect(frame.slice(6, 9)).toEqual([0, 255, 0]);
    expect(frame[10]).toBe(2);
  });

  it("lands in the pair the painter is aimed at", async () => {
    // A session lets a participant draw into somebody else's layers. The
    // curve was rasterised into our own pair while the operation sent for it
    // named the target's, so the author alone saw it in the wrong place.
    const { api, send } = await mountWithTool("solid", { drawType: "bezier" });
    const engine = api.drawingEngine!;
    engine.setDrawTarget("7");
    await buildCurve(send, [20, 8], [40, 8]);

    const theirs = engine.layersFor("7").background;
    const ours = engine.layers.background;
    const painted = (layer: Uint8ClampedArray) => {
      let count = 0;
      for (let i = 3; i < layer.length; i += 4) if (layer[i] > 0) count++;
      return count;
    };
    expect(painted(theirs)).toBeGreaterThan(30);
    expect(painted(ours)).toBe(0);
  });

  it("commits only on the third release, and records NEO's frame", async () => {
    const { api, send } = await mountWithTool("solid", { drawType: "bezier" });
    const layer = api.drawingEngine!.layers.background;
    const at = (x: number, y: number) => layer[(y * W + x) * 4 + 3];

    await send("pointerdown", 8, 30);
    await act(async () => { await sleep(20); });
    await send("pointermove", 50, 30);
    await send("pointerup", 50, 30);
    // The chord is only a preview so far -- nothing on the canvas yet
    expect(at(29, 30)).toBe(0);

    await send("pointerdown", 16, 6);
    await send("pointerup", 16, 6);
    expect(at(29, 30)).toBe(0);

    await send("pointerdown", 42, 6);
    await send("pointerup", 42, 6);
    // Third release commits
    expect(at(29, 12)).toBeGreaterThan(0);

    const bytes = new Uint8Array(await api.getReplayBlob().arrayBuffer());
    const { decodePCH } = await import("./NeoReplay");
    const frame = decodePCH(bytes)!.items.at(-1)!;
    expect(frame[0]).toBe("bezier");
    // pushCurrent occupies 2..10, then lineType and the four points in
    // NEO's order: start, both handles, end.
    expect(frame[11]).toBe(1);
    expect(frame.slice(12, 20)).toEqual([8, 30, 16, 6, 42, 6, 50, 30]);
  });

  it("bows away from the chord, so it is a curve and not a line", async () => {
    const { api, send } = await mountWithTool("solid", { drawType: "bezier" });
    const layer = api.drawingEngine!.layers.background;
    const at = (x: number, y: number) => layer[(y * W + x) * 4 + 3];

    await buildCurve(send, [16, 6], [42, 6]);

    // Handles pull the middle up, away from the straight chord at y=30
    expect(at(29, 30)).toBe(0);
    let bowed = false;
    for (let y = 8; y < 22; y++) if (at(29, y) > 0) bowed = true;
    expect(bowed).toBe(true);
    // Both endpoints are still on the curve
    expect(at(8, 30)).toBeGreaterThan(0);
    expect(at(50, 30)).toBeGreaterThan(0);
  });

  it("replays to the same pixels it drew", async () => {
    const { api, send } = await mountWithTool("solid", { drawType: "bezier" });
    await buildCurve(send, [14, 4], [44, 8]);

    const { decodePCH, NeoReplay } = await import("./NeoReplay");
    const { BufferSurface } = await import("./PixelSurface");
    const decoded = decodePCH(
      new Uint8Array(await api.getReplayBlob().arrayBuffer())
    )!;
    const replay = new NeoReplay(W, H);
    const buffers = [new Uint8ClampedArray(W * H * 4), new Uint8ClampedArray(W * H * 4)];
    replay.painter.surfaces = [
      new BufferSurface(buffers[0], W, H),
      new BufferSurface(buffers[1], W, H),
    ];
    await replay.playAll(decoded.items);

    const ours = api.drawingEngine!.layers.background;
    const theirs = replay.getLayerPixels(0);
    let painted = 0;
    let disagreed = 0;
    for (let i = 3; i < ours.length; i += 4) {
      if (ours[i] > 0 && theirs[i] > 0) painted++;
      if (ours[i] > 0 !== theirs[i] > 0) disagreed++;
    }
    expect(painted).toBeGreaterThan(40);
    expect(disagreed).toBe(0);
  });
});

describe("copy and paste", () => {
  it("copies a region and pastes it elsewhere", async () => {
    const { api, send } = await mountWithTool("rectFill");
    const layer = api.drawingEngine!.layers.background;

    // Something to copy, in the top-left
    await send("pointerdown", 4, 4);
    await act(async () => { await sleep(20); });
    await send("pointermove", 16, 16);
    await send("pointerup", 16, 16);
    const at = (x: number, y: number) => layer[(y * W + x) * 4 + 3];
    expect(at(10, 10)).toBeGreaterThan(0);
    expect(at(40, 26)).toBe(0);

    // Copy leaves the canvas untouched
    const before = new Uint8ClampedArray(layer);
    api.drawingEngine!.applyRegionTool(
      "copy", "background", { x: 4, y: 4, width: 13, height: 13 },
      { r: 0, g: 0, b: 0, a: 255 }, 1
    );
    expect(Array.from(layer)).toEqual(Array.from(before));

    // Paste drops it at the destination
    api.drawingEngine!.applyRegionTool(
      "paste", "background", { x: 34, y: 20, width: 13, height: 13 },
      { r: 0, g: 0, b: 0, a: 255 }, 1
    );
    expect(at(40, 26)).toBeGreaterThan(0);
  });

  // The gesture -- copy handing over to paste, the drag, the frames NEO
  // reads -- is covered in copyPaste.browser.test.tsx.
});

describe("a freehand stroke", () => {
  it("records the mask and layer it was drawn through", async () => {
    // The recorder now takes both from the settings the drawing hook froze
    // at the press, the same way every shape tool does; it keeps no copy of
    // its own to go stale.
    const { api, send } = await mountWithTool("solid", {
      layerType: "foreground", maskType: 3, maskColor: "#0000ff",
    });
    await send("pointerdown", 8, 8);
    await act(async () => { await sleep(20); });
    await send("pointermove", 20, 8);
    await send("pointerup", 20, 8);

    const frame = await lastFrame(api);
    expect(frame[0]).toBe("freeHand");
    expect(frame[1]).toBe(1);
    expect(frame.slice(6, 9)).toEqual([0, 0, 255]);
    expect(frame[10]).toBe(3);
  });
});

describe("undo while the pen is down", () => {
  it("does nothing until the stroke is over", async () => {
    // Offline, the snapshot popped was the one taken before the *previous*
    // stroke, so the mark before this one vanished from under the pen while
    // this one went on. NEO answers the key mid-stroke; this painter does
    // not, by choice.
    const { api, send } = await mountWithTool("solid");
    const layer = api.drawingEngine!.layers.background;
    const at = (x: number, y: number) => layer[(y * W + x) * 4 + 3];

    await send("pointerdown", 8, 8);
    await act(async () => { await sleep(20); });
    await send("pointermove", 20, 8);
    await send("pointerup", 20, 8);
    expect(at(14, 8)).toBeGreaterThan(0);

    await send("pointerdown", 8, 24);
    await act(async () => { await sleep(20); });
    await send("pointermove", 20, 24);
    await act(async () => { api.undo(); });
    await send("pointerup", 20, 24);

    expect(at(14, 8)).toBeGreaterThan(0);
    expect(at(14, 24)).toBeGreaterThan(0);
    // And once the pen is up, undo takes the stroke that was just finished.
    await act(async () => { api.undo(); });
    expect(at(14, 24)).toBe(0);
    expect(at(14, 8)).toBeGreaterThan(0);
  });
});

describe("the fill tool, in a session", () => {
  it("emits the fill in the turn it floods", async () => {
    // The coverage used to be compressed through the browser's streams, so
    // the operation reached the fork a few milliseconds after the pixels
    // reached the canvas -- long enough for the next emission, or a
    // checkpoint request, to get in ahead of it.
    const emitted: PainterOperation["kind"][] = [];
    const { canvas } = await mountWithTool("fill", {}, (operation) => {
      emitted.push(operation.kind);
    });
    const box = canvas.getBoundingClientRect();
    canvas.dispatchEvent(new PointerEvent("pointerdown", {
      pointerId: 1, pointerType: "mouse", button: 0, buttons: 1,
      clientX: box.left + 12, clientY: box.top + 12,
      bubbles: true, cancelable: true,
    }));
    // Nothing awaited: the press is still on the stack.
    expect(emitted).toContain("fill-region");
  });
});

describe("the text tool", () => {
  it("takes its font size from the pen, with no picker", async () => {
    const { fontSizeForBrush, TEXT_FONT_FAMILY } = await import("./tools");
    // NEO's updateInputText: round(d * 55 / 28 + 7)
    expect(fontSizeForBrush(1)).toBe(9);
    expect(fontSizeForBrush(28)).toBe(62);
    expect(TEXT_FONT_FAMILY).toBe("Arial");
  });

  it("draws text onto the layer and records NEO's frame", async () => {
    const { api } = await mountWithTool("text");
    const engine = api.drawingEngine!;
    const layer = engine.layers.background;
    expect(layer.every((v) => v === 0)).toBe(true);

    engine.drawText(
      "background", 4, 20, { r: 0, g: 0, b: 0 }, 1, "Hi", "20px", "Arial"
    );
    expect(layer.some((v) => v !== 0)).toBe(true);

    api.recordText("background", 4, 20, 0x000000, 1, "Hi", "20px", "Arial");
    const { decodePCH } = await import("./NeoReplay");
    const frame = decodePCH(
      new Uint8Array(await api.getReplayBlob().arrayBuffer())
    )!.items.at(-1)!;
    expect(frame).toEqual(["text", 0, 4, 20, 0, 1, "Hi", "20px", "Arial"]);
  });

  it("draws nothing for empty text", async () => {
    const { api } = await mountWithTool("text");
    const engine = api.drawingEngine!;
    engine.drawText("background", 4, 20, { r: 0, g: 0, b: 0 }, 1, "", "20px", "Arial");
    expect(engine.layers.background.every((v) => v === 0)).toBe(true);
  });
});
