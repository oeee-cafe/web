import { describe, expect, it } from "vitest";
import { act } from "react";
import { describeDifference, firstPixelDifference } from "../test/neoHarness";
import {
  W,
  mountPainter,
  neoRendering,
  sleep,
} from "../test/offlinePainterHarness";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/*
 * Copy and paste as NEO does them.
 *
 * NEO has no paste button. Dragging out a copy grabs the rectangle and puts
 * the painter into paste mode (CopyTool.doEffect); a press anywhere then
 * drags the copy by however far the pointer moves (PasteTool.moveHandler),
 * release stamps it there and returns to copy (upHandler), and Escape puts
 * it down unpasted (keyDownHandler).
 *
 * Ours used to make paste a separate tool that dragged out a *rectangle*,
 * and pasted the clipboard into whatever size that rectangle was. NEO's
 * paste indexes the clipboard with the destination's own stride, so any
 * rectangle but the copied one sheared the image, and a larger one read
 * past the end of the clipboard and wrote transparent pixels -- erasing.
 */

describe("copy and paste, NEO's way", () => {
  it("switches to paste when the copy is finished, and back once it is dropped", async () => {
    const p = await mountPainter("copy");

    await p.drag(4, 4, 16, 16);
    expect(p.tools).toEqual(["paste"]);
    expect(p.handle.tool).toBe("paste");

    await p.drag(30, 30, 40, 30);
    expect(p.tools).toEqual(["paste", "copy"]);
    expect(p.handle.tool).toBe("copy");
  });

  it("moves the copy by the drag, at the size it was copied", async () => {
    const p = await mountPainter("rectFill");
    await p.drag(4, 4, 16, 16);
    expect(p.alphaAt(10, 10)).toBeGreaterThan(0);
    expect(p.alphaAt(30, 16)).toBe(0);

    await p.selectTool("copy");
    await p.drag(4, 4, 16, 16);

    // Pressed nowhere near the copy: only the distance travelled counts.
    await p.drag(30, 30, 50, 36);

    // (4,4) moved by (20,6), still 13 x 13
    expect(p.alphaAt(24, 10)).toBeGreaterThan(0);
    expect(p.alphaAt(36, 22)).toBeGreaterThan(0);
    expect(p.alphaAt(37, 22)).toBe(0);
    expect(p.alphaAt(36, 23)).toBe(0);
    // Copying takes nothing away from where it came from
    expect(p.alphaAt(10, 10)).toBeGreaterThan(0);
  });

  it("records NEO's own frames, which NEO renders to the same pixels", async () => {
    const p = await mountPainter("rectFill");
    await p.drag(4, 4, 16, 16);
    await p.selectTool("copy");
    await p.drag(4, 4, 16, 16);
    await p.drag(30, 30, 50, 36);

    const items = await p.frames();
    // ["copy", layer, x, y, w, h] and ["paste", layer, x, y, w, h, dx, dy]:
    // the source rectangle and the offset, not the rectangle it landed on.
    expect(items.at(-2)).toEqual(["copy", 0, 4, 4, 13, 13]);
    expect(items.at(-1)).toEqual(["paste", 0, 4, 4, 13, 13, 20, 6]);

    const ours = new Uint8ClampedArray(p.layer());
    const neo = await neoRendering(p.handle.api!.getReplayBlob());
    expect(firstPixelDifference(ours, neo), describeDifference(ours, neo, W)).toBe(-1);
  });

  /*
   * NEO's paste replaces the rectangle outright; it does not composite. A
   * copy of empty canvas pasted over a drawing cuts a hole in it. That
   * surprises people, and it is also what every NEO replay already on disk
   * does, so it is pinned here rather than "fixed".
   */
  it("pastes transparent pixels too, as NEO does", async () => {
    const p = await mountPainter("rectFill");
    await p.drag(30, 10, 45, 25);
    expect(p.alphaAt(36, 16)).toBeGreaterThan(0);

    await p.selectTool("copy");
    await p.drag(2, 2, 10, 8); // nothing drawn here
    await p.drag(2, 2, 32, 12); // drop it onto the filled square

    expect(p.alphaAt(36, 16)).toBe(0);
    const neo = await neoRendering(p.handle.api!.getReplayBlob());
    const ours = new Uint8ClampedArray(p.layer());
    expect(firstPixelDifference(ours, neo), describeDifference(ours, neo, W)).toBe(-1);
  });

  /*
   * What is on screen at each step, as NEO has it.
   *
   * EffectToolBase draws nothing on the press and the selection rectangle
   * on each move. Its upHandler redraws the display for every tool but
   * paste, so after a copy that rectangle stays up. PasteTool.downHandler
   * then XORs its own outline in the same place, which erases it: the press
   * shows nothing. moveHandler redraws with the copy on top, and nothing is
   * drawn around the copy while it moves.
   */
  it("shows what NEO shows, when NEO shows it", async () => {
    const p = await mountPainter("copy");
    const rect = { x: 4, y: 4, width: 13, height: 13 };

    await p.send("pointerdown", 4, 4);
    expect(p.regionPreviews.filter(Boolean)).toEqual([]);

    await act(async () => { await sleep(20); });
    await p.send("pointermove", 16, 16);
    expect(p.regionPreviews.at(-1)).toEqual(rect);

    await p.send("pointerup", 16, 16);
    expect(p.previews.at(-1)).toEqual({ kind: "marks", rects: [rect] });

    await p.send("pointerdown", 30, 30);
    expect(p.previews.at(-1)).toBeNull();

    await act(async () => { await sleep(20); });
    await p.send("pointermove", 37, 33);
    const moving = p.previews.at(-1);
    expect(moving).toMatchObject({ kind: "floating", x: 11, y: 7 });
    expect(moving?.kind === "floating" && moving.image.width).toBe(13);

    await p.send("pointerup", 37, 33);
    expect(p.previews.at(-1)).toBeNull();
  });

  it("leaves no outline after a copy that was only clicked, and draws one on the press", async () => {
    const p = await mountPainter("copy");
    await p.send("pointerdown", 8, 8);
    await p.send("pointerup", 8, 8);
    expect(p.handle.tool).toBe("paste");
    expect(p.previews.at(-1)).toBeNull();

    // With nothing left over to cancel, PasteTool's XOR is simply drawn.
    await p.send("pointerdown", 20, 20);
    expect(p.previews.at(-1)).toEqual({
      kind: "marks",
      rects: [{ x: 8, y: 8, width: 1, height: 1 }],
    });
  });

  /*
   * NEO measures the drag on its unrounded mouse position and floors the
   * difference. Rounding each end first -- the old way -- would put this
   * 1.2 pixel drag two pixels over instead of one.
   */
  it("floors the offset on the unrounded pointer, as NEO does", async () => {
    const p = await mountPainter("copy");
    await p.drag(4, 4, 16, 16);

    await p.send("pointerdown", 30.4, 30);
    await act(async () => { await sleep(20); });
    await p.send("pointermove", 31.6, 30);
    expect(p.previews.at(-1)).toMatchObject({ kind: "floating", x: 5, y: 4 });
    await p.send("pointerup", 31.6, 30);

    expect((await p.frames()).at(-1)).toEqual(["paste", 0, 4, 4, 13, 13, 1, 0]);
  });

  it("outlines the copy afresh when the layer changes while it waits", async () => {
    const p = await mountPainter("copy");
    await p.send("pointerdown", 8, 8);
    await p.send("pointerup", 8, 8);
    expect(p.previews.at(-1)).toBeNull();

    // LayerControl redraws the display, then PasteTool.drawCursor.
    await p.selectLayer("foreground");
    expect(p.previews.at(-1)).toEqual({
      kind: "marks",
      rects: [{ x: 8, y: 8, width: 1, height: 1 }],
    });
  });

  it("wipes the outline on undo but keeps the copy to paste", async () => {
    const p = await mountPainter("rectFill");
    await p.drag(4, 4, 16, 16);
    await p.selectTool("copy");
    await p.drag(4, 4, 16, 16);
    expect(p.previews.at(-1)).toMatchObject({ kind: "marks" });

    await act(async () => { p.handle.api!.undo(); });
    expect(p.previews.at(-1)).toBeNull();
    expect(p.handle.tool).toBe("paste");
  });

  it("puts the copy down unpasted on Escape", async () => {
    const p = await mountPainter("rectFill");
    await p.drag(4, 4, 16, 16);
    await p.selectTool("copy");
    await p.drag(4, 4, 16, 16);
    const before = new Uint8ClampedArray(p.layer());

    await act(async () => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });

    expect(p.handle.tool).toBe("copy");
    expect(p.previews.at(-1)).toBeNull();
    expect(Array.from(p.layer())).toEqual(Array.from(before));
    expect((await p.frames()).some((item) => item[0] === "paste")).toBe(false);
  });

  it("abandons the copy when another tool is chosen", async () => {
    const p = await mountPainter("copy");
    await p.drag(4, 4, 16, 16);
    expect(p.handle.tool).toBe("paste");

    await p.selectTool("solid");
    expect(p.previews.at(-1)).toBeNull();
  });

  it("empties the clipboard with the paste, so one copy is one paste", async () => {
    const p = await mountPainter("rectFill");
    await p.drag(4, 4, 16, 16);
    await p.selectTool("copy");
    await p.drag(4, 4, 16, 16);
    expect(p.handle.api!.drawingEngine!.getClipboard()).not.toBeNull();

    await p.drag(30, 30, 50, 36);
    expect(p.handle.api!.drawingEngine!.getClipboard()).toBeNull();
  });
});

/*
 * A collaborative session keeps one clipboard per participant by swapping
 * `getClipboard`/`setClipboard` around every operation. Those used to serve
 * a field copy and paste never touched, so the swap moved an empty box and
 * everyone shared the real clipboard. This drives the real engine, not the
 * stand-in the history's own tests use, which had a working clipboard of
 * its own and so could not see it.
 */
describe("the clipboard a shared session swaps", () => {
  it("is the one copy fills and paste reads", async () => {
    const p = await mountPainter("rectFill");
    await p.drag(4, 4, 16, 16);
    const engine = p.handle.api!.drawingEngine!;
    const black = { r: 0, g: 0, b: 0, a: 255 };
    const source = { x: 4, y: 4, width: 13, height: 13 };
    const dest = { x: 30, y: 20, width: 13, height: 13 };

    engine.applyRegionTool("copy", "background", source, black, 1);
    const mine = engine.getClipboard();
    expect(mine?.width).toBe(13);
    expect(mine?.height).toBe(13);

    // Somebody else's turn, with nothing copied: nothing to paste.
    engine.setClipboard(null);
    engine.applyRegionTool("paste", "background", dest, black, 1);
    expect(p.alphaAt(36, 26)).toBe(0);

    // Back to mine: it is still there to paste.
    engine.setClipboard(mine);
    engine.applyRegionTool("paste", "background", dest, black, 1);
    expect(p.alphaAt(36, 26)).toBeGreaterThan(0);
  });
});

describe("drawing paste mode", () => {
  const SIZE = 10;
  const backdrop = {
    width: SIZE,
    height: SIZE,
    scale: 1,
    layers: [new Uint8ClampedArray(SIZE * SIZE * 4)],
  };
  const overlay = () => {
    const canvas = document.createElement("canvas");
    canvas.width = SIZE;
    canvas.height = SIZE;
    return canvas.getContext("2d")!;
  };
  const pixel = (ctx: CanvasRenderingContext2D, x: number, y: number) =>
    Array.from(ctx.getImageData(x, y, 1, 1).data);

  /*
   * The report that started this: ours drew a border round the copy while
   * it moved, and NEO draws the copy alone. Its edge pixels are the copy's
   * own colour, not an inverted outline.
   */
  it("draws the moving copy with no border round it", async () => {
    const { drawPastePreview } = await import("./regionPreview");
    const image = new ImageData(3, 3);
    for (let i = 0; i < image.data.length; i += 4) image.data.set([200, 0, 0, 255], i);

    const ctx = overlay();
    drawPastePreview(ctx, { kind: "floating", image, x: 2, y: 2 }, backdrop);

    expect(pixel(ctx, 2, 2)).toEqual([200, 0, 0, 255]);
    expect(pixel(ctx, 4, 4)).toEqual([200, 0, 0, 255]);
    expect(pixel(ctx, 5, 5)).toEqual([0, 0, 0, 0]);
  });

  it("shows an empty pixel of the copy as white, as NEO's tempCanvas does", async () => {
    const { drawPastePreview } = await import("./regionPreview");
    const image = new ImageData(2, 1);
    image.data.set([10, 20, 30, 255], 0); // the second pixel is left empty

    const ctx = overlay();
    drawPastePreview(ctx, { kind: "floating", image, x: 0, y: 0 }, backdrop);
    expect(pixel(ctx, 0, 0)).toEqual([10, 20, 30, 255]);
    expect(pixel(ctx, 1, 0)).toEqual([255, 255, 255, 255]);
  });

  it("erases an outline drawn twice in the same place, as a second XOR does", async () => {
    const { drawPastePreview } = await import("./regionPreview");
    const rect = { x: 2, y: 2, width: 4, height: 4 };

    const once = overlay();
    drawPastePreview(once, { kind: "marks", rects: [rect] }, backdrop);
    expect(pixel(once, 2, 2)[3]).toBe(255);

    const twice = overlay();
    drawPastePreview(twice, { kind: "marks", rects: [rect, rect] }, backdrop);
    expect(pixel(twice, 2, 2)).toEqual([0, 0, 0, 0]);
  });
});
