import { describe, expect, it } from "vitest";
import { createCanonicalPainter } from "../test/neoHarness";
import {
  drawBezierPreview,
  drawLinePreview,
  drawRegionPreview,
  type Backdrop,
} from "./regionPreview";
import type { RegionTool, ToolId } from "./tools";
import { previewBackdrop } from "./previewBackdrop";
import { DrawingEngine } from "../DrawingEngine";

const W = 72;
const H = 56;

function context(): CanvasRenderingContext2D {
  const canvas = document.createElement("canvas");
  canvas.width = W;
  canvas.height = H;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) throw new Error("no 2d context");
  return ctx;
}

function artwork(): CanvasRenderingContext2D {
  const ctx = context();
  ctx.fillStyle = "rgb(187,93,41)";
  ctx.fillRect(0, 0, W / 2, H);
  ctx.fillStyle = "rgb(31,142,203)";
  ctx.fillRect(W / 2, 0, W / 2, H);
  return ctx;
}

function backdrop(ctx: CanvasRenderingContext2D): Backdrop {
  return {
    width: W,
    height: H,
    layers: [new Uint8ClampedArray(ctx.getImageData(0, 0, W, H).data)],
  };
}

function ours(
  draw: (ctx: CanvasRenderingContext2D, back: Backdrop) => void
): Uint8ClampedArray {
  const art = artwork();
  const overlay = context();
  draw(overlay, backdrop(art));
  art.drawImage(overlay.canvas, 0, 0);
  return new Uint8ClampedArray(art.getImageData(0, 0, W, H).data);
}

function canonical(
  draw: (painter: ReturnType<typeof createCanonicalPainter>["painter"], ctx: CanvasRenderingContext2D) => void
): Uint8ClampedArray {
  const cp = createCanonicalPainter(W, H);
  const ctx = artwork();
  draw(cp.painter, ctx);
  return new Uint8ClampedArray(ctx.getImageData(0, 0, W, H).data);
}

function expectPixels(actual: Uint8ClampedArray, expected: Uint8ClampedArray) {
  expect(actual).toEqual(expected);
}

describe("interactive previews against canonical NEO", () => {
  it("draws a white region outline over an opaque black background layer", () => {
    const black = new Uint8ClampedArray(W * H * 4);
    for (let i = 0; i < black.length; i += 4) black[i + 3] = 255;
    const transparentForeground = new Uint8ClampedArray(W * H * 4);
    const overlay = context();

    drawRegionPreview(
      overlay,
      { x: 7, y: 5, width: 51, height: 39 },
      { width: W, height: H, layers: [black, transparentForeground] },
      "eraseRect"
    );

    const topEdge = overlay.getImageData(7, 5, 1, 1).data;
    expect(Array.from(topEdge)).toEqual([255, 255, 255, 255]);
  });

  it("derives the preview from the mounted layer stack", () => {
    const engine = new DrawingEngine(W, H);
    const background = context();
    const foreground = context();
    background.fillStyle = "black";
    background.fillRect(0, 0, W, H);
    engine.attachDOMCanvases(background.canvas, foreground.canvas);

    // The mounted canvas is deliberately newer than the raw buffer. NEO's
    // destination-canvas XOR follows what is rendered, so ours must too.
    const overlay = context();
    drawRegionPreview(
      overlay,
      { x: 7, y: 5, width: 51, height: 39 },
      previewBackdrop(engine, W, H, 1, true, true),
      "eraseRect"
    );

    expect(Array.from(overlay.getImageData(7, 5, 1, 1).data)).toEqual([
      255, 255, 255, 255,
    ]);
  });

  it("composites every visible participant, latest joiner at the bottom", () => {
    // What is on screen in a session is the whole stack. An outline computed
    // against our own pair alone came out wrong wherever somebody above us
    // had drawn.
    const engine = new DrawingEngine(W, H);
    engine.setLocalOwner("1");
    engine.attachDOMCanvases(context().canvas, context().canvas, "1");
    const theirs = context();
    theirs.fillStyle = "black";
    theirs.fillRect(0, 0, W, H);
    engine.attachDOMCanvases(theirs.canvas, context().canvas, "2");

    const both = previewBackdrop(engine, W, H, 1, true, true);
    // Participant 2's pair first -- they joined later and sit underneath.
    expect(both.layers).toHaveLength(4);
    expect(both.layers[0][3]).toBe(255);
    expect(both.layers[2][3]).toBe(0);

    const hidden = previewBackdrop(engine, W, H, 1, true, true, new Set(["2"]));
    expect(hidden.layers).toHaveLength(2);
    expect(hidden.layers.every((layer) => layer[3] === 0)).toBe(true);
  });

  it("draws the straight-line cursor exactly", () => {
    const from = { x: 3, y: 46 };
    const to = { x: 68, y: 4 };
    expectPixels(
      ours((ctx, back) => drawLinePreview(ctx, from, to, back)),
      canonical((p, ctx) => p.drawXORLine(ctx, from.x, from.y, to.x, to.y))
    );
  });

  it("draws every region cursor exactly", () => {
    const rect = { x: 7, y: 5, width: 51, height: 39 };
    const cases: Array<[ToolId, boolean, boolean]> = [
      ["rect", false, false],
      ["rectFill", false, true],
      ["ellipse", true, false],
      ["ellipseFill", true, true],
    ];

    for (const [tool, ellipse, fill] of cases) {
      const actual = ours((ctx, back) =>
        drawRegionPreview(ctx, rect, back, tool)
      );
      const expected = canonical((p, ctx) => {
        const method = ellipse ? p.drawXOREllipse : p.drawXORRect;
        method.call(p, ctx, rect.x, rect.y, rect.width, rect.height, fill);
      });
      expect(actual, tool).toEqual(expected);
    }
  });

  it("keeps every region cursor pixel-perfect at zoom", () => {
    const scale = 2;
    const rect = { x: 7, y: 5, width: 25, height: 19 };
    const tools: RegionTool[] = [
      "eraseRect", "blurRect", "merge", "flipH", "flipV", "turn",
      "copy", "paste", "rect", "rectFill", "ellipse", "ellipseFill",
    ];

    const source = artwork();
    const back: Backdrop = {
      width: W,
      height: H,
      scale,
      layers: [new Uint8ClampedArray(source.getImageData(0, 0, W, H).data)],
    };

    for (const tool of tools) {
      const overlay = document.createElement("canvas");
      overlay.width = W * scale;
      overlay.height = H * scale;
      const overlayCtx = overlay.getContext("2d", { willReadFrequently: true })!;
      drawRegionPreview(overlayCtx, rect, back, tool);

      const actualCtx = document.createElement("canvas").getContext("2d")!;
      actualCtx.canvas.width = W * scale;
      actualCtx.canvas.height = H * scale;
      actualCtx.fillStyle = "rgb(187,93,41)";
      actualCtx.fillRect(0, 0, W, H * scale);
      actualCtx.fillStyle = "rgb(31,142,203)";
      actualCtx.fillRect(W, 0, W, H * scale);
      actualCtx.drawImage(overlay, 0, 0);

      const expectedCtx = document.createElement("canvas").getContext("2d")!;
      expectedCtx.canvas.width = W * scale;
      expectedCtx.canvas.height = H * scale;
      expectedCtx.putImageData(actualCtx.getImageData(0, 0, W * scale, H * scale), 0, 0);
      // Restore the un-previewed display artwork before asking canonical NEO.
      expectedCtx.fillStyle = "rgb(187,93,41)";
      expectedCtx.fillRect(0, 0, W, H * scale);
      expectedCtx.fillStyle = "rgb(31,142,203)";
      expectedCtx.fillRect(W, 0, W, H * scale);

      const cp = createCanonicalPainter(W * scale, H * scale);
      const ellipse = tool === "ellipse" || tool === "ellipseFill";
      const fill = tool === "rectFill" || tool === "ellipseFill";
      const method = ellipse ? cp.painter.drawXOREllipse : cp.painter.drawXORRect;
      method.call(
        cp.painter, expectedCtx,
        rect.x * scale, rect.y * scale,
        rect.width * scale, rect.height * scale, fill
      );

      expect(
        new Uint8ClampedArray(actualCtx.getImageData(0, 0, W * scale, H * scale).data),
        tool
      ).toEqual(expectedCtx.getImageData(0, 0, W * scale, H * scale).data);
    }
  });

  const points = [8, 38, 18, 4, 55, 50, 65, 15];
  const style = { color: [73, 19, 211, 96] as [number, number, number, number], width: 7 };

  function canonicalBezier(step: 1 | 2): Uint8ClampedArray {
    return canonical((p, ctx) => {
      const [x0, y0, x1, y1, x2, y2, x3, y3] = points;
      p._currentColor = [...style.color];
      p._currentWidth = style.width;
      if (step === 1) {
        p.drawXORLine(ctx, x0, y0, x1, y1);
        p.drawXOREllipse(ctx, x1 - 4, y1 - 4, 8, 8);
        p.drawXOREllipse(ctx, x0 - 4, y0 - 4, 8, 8);
        p.drawBezier(ctx, x0, y0, x1, y1, x1, y1, x3, y3, 1, false, true);
      } else {
        p.drawXORLine(ctx, x3, y3, x2, y2);
        p.drawXOREllipse(ctx, x2 - 4, y2 - 4, 8, 8);
        p.drawXORLine(ctx, x0, y0, x1, y1);
        p.drawXOREllipse(ctx, x1 - 4, y1 - 4, 8, 8);
        p.drawXOREllipse(ctx, x0 - 4, y0 - 4, 8, 8);
        p.drawBezier(ctx, x0, y0, x1, y1, x2, y2, x3, y3, 1, false, true);
      }
    });
  }

  it("draws the first bezier-handle phase exactly", () => {
    expectPixels(
      ours((ctx, back) => drawBezierPreview(ctx, points, back, 1, style)),
      canonicalBezier(1)
    );
  });

  it("draws the second bezier-handle phase exactly", () => {
    expectPixels(
      ours((ctx, back) => drawBezierPreview(ctx, points, back, 2, style)),
      canonicalBezier(2)
    );
  });
});
