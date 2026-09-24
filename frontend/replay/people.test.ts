import { describe, expect, it } from "vitest";
import type { LogRow } from "./logRows";
import { participantStats } from "./people";

let seq = 0;
const row = (sessionId: number, kind: string, at: number, target: number | null = sessionId): LogRow => {
  seq++;
  const mark = !["pointerup", "resetPoint"].includes(kind);
  return {
    index: seq - 1,
    seq,
    at,
    kind,
    actor: String(sessionId),
    sessionId,
    target: ["stroke", "fill"].includes(kind) ? target : null,
    onOther: ["stroke", "fill"].includes(kind) && target !== sessionId,
    summary: "",
    drawable: mark,
    position: 0,
  };
};

describe("who did what", () => {
  const rows = [
    row(1, "undoPoint", 0),
    row(1, "stroke", 0),
    row(1, "pointerup", 10),
    row(2, "undoPoint", 50),
    row(2, "stroke", 50, 1),
    row(2, "fill", 60),
    row(1, "undo", 90),
    row(1, "redo", 100),
  ];
  const [miro, oeee] = participantStats(rows, 4);

  /** An undo boundary opens every gesture; counted, it would double every
   * mark. */
  it("counts marks, not the bookkeeping around them", () => {
    expect(miro).toMatchObject({ sessionId: 1, marks: 1, strokes: 1, undos: 1, redos: 1 });
    expect(oeee).toMatchObject({ sessionId: 2, marks: 2, strokes: 1 });
  });

  it("counts a mark on somebody else's layer against both of them", () => {
    expect(oeee.onOthers).toBe(1);
    expect(miro.byOthers).toBe(1);
    expect(miro.onOthers).toBe(0);
  });

  it("knows when each was first and last there", () => {
    expect([miro.firstAt, miro.lastAt]).toEqual([0, 100]);
    expect([oeee.firstAt, oeee.lastAt]).toEqual([50, 60]);
  });

  /** The last moment falls in the last slice rather than one past it. */
  it("spreads marks over the recording's span", () => {
    expect(miro.activity).toEqual([1, 0, 0, 0]);
    expect(oeee.activity).toEqual([0, 0, 2, 0]);
  });
});
