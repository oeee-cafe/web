import { describe, expect, it } from "vitest";
import { describeDifference, firstPixelDifference } from "../test/neoHarness";
import { W, mountPainter, neoRendering } from "../test/offlinePainterHarness";
import type { ToolId } from "./tools";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/*
 * Every shape the region tools draw, drawn the way a person draws it -- a
 * drag through the offline hook -- and then replayed by NEO itself.
 *
 * NEO's `fill` frame ends with which shape it is, and doFill draws nothing
 * for a type it does not know. The gesture path left that slot off from the
 * day these tools were ported, so every rectangle and ellipse replayed as
 * blank in NEO and in our own viewer while the canvas showed it. The
 * existing round-trip suite drives the recorder directly and passes the type
 * itself, which is exactly why it never saw this: the frames it checked were
 * not the frames a drag produces.
 */
describe("shapes drawn with a drag, replayed by NEO", () => {
  const shapes: ToolId[] = ["rect", "rectFill", "ellipse", "ellipseFill"];

  for (const tool of shapes) {
    it(`${tool} renders the same in NEO as on the canvas`, async () => {
      const p = await mountPainter(tool);
      await p.drag(6, 5, 40, 30);

      const ours = new Uint8ClampedArray(p.layer());
      expect(ours.some((v) => v !== 0), "nothing was drawn").toBe(true);

      const items = await p.frames();
      // ["fill", layer, 9 drawing-state slots, x, y, w, h, type]
      expect(items.at(-1)).toHaveLength(16);

      const neo = await neoRendering(p.handle.api!.replay!.getReplayBlob());
      expect(
        firstPixelDifference(ours, neo),
        describeDifference(ours, neo, W)
      ).toBe(-1);
    });
  }
});
