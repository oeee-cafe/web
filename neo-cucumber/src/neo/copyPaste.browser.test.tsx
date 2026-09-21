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

  it("outlines the copy, then shows it moving, then clears", async () => {
    const p = await mountPainter("copy");
    await p.drag(4, 4, 16, 16);
    // Straight after the copy: outlined where it was taken from, not drawn
    expect(p.previews.at(-1)).toMatchObject({ x: 4, y: 4, dragging: false });

    await p.send("pointerdown", 30, 30);
    await act(async () => { await sleep(20); });
    await p.send("pointermove", 37, 33);
    expect(p.previews.at(-1)).toMatchObject({ x: 11, y: 7, dragging: true });
    expect(p.previews.at(-1)!.image.width).toBe(13);

    await p.send("pointerup", 37, 33);
    expect(p.previews.at(-1)).toBeNull();
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
