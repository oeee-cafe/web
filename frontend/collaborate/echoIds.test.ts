import { describe, expect, it } from "vitest";
import { PendingEchoes } from "./echoIds";

const BOUNDARY = "1903";

describe("matching echoes to the operations this connection sent", () => {
  it("names repeated payloads in the order they were sent", () => {
    const pending = new PendingEchoes();
    pending.record(BOUNDARY, "a");
    pending.record(BOUNDARY, "b");
    expect(pending.claim(BOUNDARY)).toBe("a");
    expect(pending.claim(BOUNDARY)).toBe("b");
    expect(pending.size).toBe(0);
  });

  it("keeps later boundaries on their own names after the server drops one", () => {
    const pending = new PendingEchoes();
    pending.record(BOUNDARY, "dropped");
    pending.record("stroke", "stroke");
    pending.record(BOUNDARY, "kept");
    // The first boundary never comes back; the stroke's echo passes it.
    expect(pending.claim("stroke")).toBe("stroke");
    expect(pending.claim(BOUNDARY)).toBe("kept");
    expect(pending.size).toBe(0);
  });

  it("answers an operation it never sent with its wire id and forgets nothing", () => {
    const pending = new PendingEchoes();
    pending.record(BOUNDARY, "a");
    expect(pending.claim("from-another-tab")).toBe("from-another-tab");
    expect(pending.claim(BOUNDARY)).toBe("a");
  });

  it("forgets everything on clear", () => {
    const pending = new PendingEchoes();
    pending.record(BOUNDARY, "a");
    pending.clear();
    expect(pending.claim(BOUNDARY)).toBe(BOUNDARY);
  });
});
