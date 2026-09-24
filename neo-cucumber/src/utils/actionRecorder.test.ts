import { describe, expect, it } from "vitest";
import { decompressFromUint8Array } from "lz-string";
import { ActionRecorder } from "./ActionRecorder";
import { lineTypeForBrush } from "../hooks/drawingModes/ReplayRecorder";

// Decode a replay blob the way Neo.decodePCH does (neo/src/actions.js)
const decodeReplay = async (blob: Blob) => {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  const header = bytes.subarray(0, 12);
  return {
    magic: String.fromCharCode(...header.subarray(0, 4)),
    width: header[4] + header[5] * 0x100,
    height: header[6] + header[7] * 0x100,
    items: JSON.parse(decompressFromUint8Array(bytes.subarray(12)) as string),
  };
};

// A freeHand frame shaped like Neo's, with a tag in place of the trailing
// coordinates so strokes stay identifiable through a round trip.
const recordStroke = (recorder: ActionRecorder, tag: string) => {
  recorder.step();
  recorder.push(
    "freeHand", 0, 0, 0, 0, 255, 0, 0, 0, 2, 0, 1, 0, 0, 0, 0, tag
  );
};

const strokeTags = (items: unknown[][]) =>
  items.filter((item) => item[0] === "freeHand").map((item) => item[16]);

describe("ActionRecorder", () => {
  it("uses NEO's line type for every stroked brush", () => {
    const brushes = [
      "solid",
      "eraser",
      "brush",
      "halftone",
      "dodge",
      "burn",
      "blur",
    ] as const;

    expect(brushes.map(lineTypeForBrush)).toEqual([1, 2, 3, 4, 5, 6, 7]);
  });

  it("writes a PCH header Neo.decodePCH accepts", async () => {
    const recorder = new ActionRecorder();
    recordStroke(recorder, "s0");

    const { magic, width, height } = await decodeReplay(
      recorder.getReplayBlob(640, 480)
    );

    expect(magic).toBe("NEO ");
    expect(width).toBe(640);
    expect(height).toBe(480);
  });

  it("drops undone strokes and still appends the restore frame", async () => {
    const recorder = new ActionRecorder();
    for (const tag of ["s0", "s1", "s2", "s3", "s4"]) {
      recordStroke(recorder, tag);
    }

    // Undo the last two strokes, then save
    recorder.back();
    recorder.back();
    recorder.addRestoreAction("BG", "FG");

    const { items } = await decodeReplay(recorder.getReplayBlob(300, 300));

    expect(strokeTags(items)).toEqual(["s0", "s1", "s2"]);
    expect(items[items.length - 1]).toEqual(["restore", "BG", "FG"]);
  });

  it("keeps a single restore frame when saving twice", async () => {
    const recorder = new ActionRecorder();
    recordStroke(recorder, "s0");
    recorder.addRestoreAction("BG1", "FG1");
    recorder.addRestoreAction("BG2", "FG2");

    const { items } = await decodeReplay(recorder.getReplayBlob(300, 300));

    expect(items.filter((item: unknown[]) => item[0] === "restore")).toEqual([
      ["restore", "BG2", "FG2"],
    ]);
    expect(strokeTags(items)).toEqual(["s0"]);
  });

  it("redo restores an undone stroke before saving", async () => {
    const recorder = new ActionRecorder();
    recordStroke(recorder, "s0");
    recordStroke(recorder, "s1");
    recorder.back();
    recorder.forward();
    recorder.addRestoreAction("BG", "FG");

    const { items } = await decodeReplay(recorder.getReplayBlob(300, 300));

    expect(strokeTags(items)).toEqual(["s0", "s1"]);
  });

  it("counts only actions that survive undo", () => {
    const recorder = new ActionRecorder();
    recordStroke(recorder, "s0");
    recordStroke(recorder, "s1");
    recordStroke(recorder, "s2");
    expect(recorder.getActionCount()).toBe(3);

    recorder.back();
    expect(recorder.getActionCount()).toBe(2);

    // The restore frame counts too, which is why callers subtract it
    recorder.addRestoreAction("BG", "FG");
    expect(recorder.getActionCount()).toBe(3);
  });

  /**
   * The mask travels in slots 6..8 and 10 of every verb that carries the
   * drawing state. These slots existed all along and were written as zeros,
   * so a mask drawn on the canvas would have replayed without one -- exactly
   * the canvas/replay divergence this codebase cares most about.
   */
  describe("the mask a stroke was drawn through", () => {
    const mask = { r: 18, g: 52, b: 86, type: 2 };

    it("lands in NEO's slots on a line", async () => {
      const recorder = new ActionRecorder();
      recorder.pushLine(
        1, 1, { x: 1, y: 2 }, { x: 3, y: 4 },
        { r: 10, g: 20, b: 30, a: 255 }, 5, mask
      );

      const { items } = await decodeReplay(recorder.getReplayBlob(300, 300));
      const frame = items[0];

      expect(frame.slice(6, 9)).toEqual([18, 52, 86]);
      expect(frame[10]).toBe(2);
      // The geometry must not have shifted along with it
      expect(frame.slice(11, 16)).toEqual([1, 1, 2, 3, 4]);
    });

    it("lands in NEO's slots on a bezier", async () => {
      const recorder = new ActionRecorder();
      recorder.pushBezier(
        1, 1, [1, 2, 3, 4, 5, 6, 7, 8],
        { r: 10, g: 20, b: 30, a: 255 }, 5, mask
      );

      const { items } = await decodeReplay(recorder.getReplayBlob(300, 300));
      const frame = items[0];

      expect(frame.slice(6, 9)).toEqual([18, 52, 86]);
      expect(frame[10]).toBe(2);
      expect(frame.slice(12, 20)).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
    });

    it("lands in NEO's slots on a region tool that carries state", async () => {
      const recorder = new ActionRecorder();
      recorder.pushRegion(
        "eraseRect", true, 0,
        { x: 7, y: 8, width: 9, height: 10 },
        { r: 10, g: 20, b: 30, a: 255 }, 5, [], mask
      );

      const { items } = await decodeReplay(recorder.getReplayBlob(300, 300));
      const frame = items[0];

      expect(frame.slice(6, 9)).toEqual([18, 52, 86]);
      expect(frame[10]).toBe(2);
      expect(frame.slice(11, 15)).toEqual([7, 8, 9, 10]);
    });

    it("defaults to no mask, so untouched call sites record zeros", async () => {
      const recorder = new ActionRecorder();
      recorder.pushLine(
        1, 1, { x: 1, y: 2 }, { x: 3, y: 4 },
        { r: 10, g: 20, b: 30, a: 255 }, 5
      );

      const { items } = await decodeReplay(recorder.getReplayBlob(300, 300));

      expect(items[0].slice(6, 9)).toEqual([0, 0, 0]);
      expect(items[0][10]).toBe(0);
    });
  });
});

describe("a recorder that was told not to record", () => {
  it("keeps nothing, whatever it is asked to keep", () => {
    const recorder = new ActionRecorder(false);
    recorder.step();
    recorder.push("floodFill", 0, 1, 2, 3);
    recorder.pushLine(
      0, 1, { x: 0, y: 0 }, { x: 4, y: 4 },
      { r: 0, g: 0, b: 0, a: 255 }, 1, { type: 0, r: 0, g: 0, b: 0 },
    );
    recorder.addRestoreAction("data:image/png;base64,AA", "data:image/png;base64,BB");
    recorder.back();
    recorder.forward();

    expect(recorder.getActionCount()).toBe(0);
    expect(recorder.isRecording).toBe(false);
  });

  it("refuses to produce a replay rather than producing an empty one", () => {
    const recorder = new ActionRecorder(false);
    expect(() => recorder.getReplayBlob(64, 48)).toThrow(/without replay recording/);
  });

  it("still records by default", () => {
    const recorder = new ActionRecorder();
    recorder.step();
    recorder.push("floodFill", 0, 1, 2, 3);
    expect(recorder.getActionCount()).toBe(1);
    expect(recorder.isRecording).toBe(true);
  });
});
