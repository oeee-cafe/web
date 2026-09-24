import { describe, expect, it } from "vitest";
import { CANVAS_Z_INDEX, participantZIndex } from "./canvasStack";

describe("canvas stacking order", () => {
  it("keeps NEO's previews and cursor above both artwork layers", () => {
    expect(CANVAS_Z_INDEX.preview).toBeGreaterThan(CANVAS_Z_INDEX.background);
    expect(CANVAS_Z_INDEX.preview).toBeGreaterThan(CANVAS_Z_INDEX.foreground);
    expect(CANVAS_Z_INDEX.cursor).toBeGreaterThan(CANVAS_Z_INDEX.preview);
  });

  // The tags were a z-30 utility once, and every participant's layers sit in
  // the thousands: somebody drawing anywhere covered everybody's pointer.
  it("puts other people's pointer tags above every layer and under our own overlays", () => {
    const topLayer = participantZIndex(0, "foreground");
    expect(CANVAS_Z_INDEX.collaborators).toBeGreaterThan(topLayer);
    expect(CANVAS_Z_INDEX.collaborators).toBeLessThan(CANVAS_Z_INDEX.preview);
  });
});
