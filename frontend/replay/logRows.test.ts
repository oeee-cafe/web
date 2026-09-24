import { describe, expect, it } from "vitest";
import type { PainterOperation } from "neo-cucumber";
import { encodePainterOperation, encodePointerUp } from "../collaborate/binaryProtocol";
import type { ArchivedEntry } from "./archiveLog";
import { elapsed, logRows } from "./logRows";

/**
 * The log is where somebody looks for the moment a drawing went wrong, so a
 * row has to say who did it and put the canvas where it was when they did.
 */

const HISTORY = "00000000-0000-0000-0000-000000000007";
const NAMES = new Map([
  [1, "miro"],
  [2, "oeee"],
]);

function entry(seq: number, payload: ArrayBuffer): ArchivedEntry {
  return { at: 1_000 + seq, seq, from: "conn", historyId: HISTORY, payload: new Uint8Array(payload) };
}

const stroke = (targetActorId?: string): PainterOperation => ({
  kind: "stroke",
  ...(targetActorId ? { targetActorId } : {}),
  layer: "foreground",
  brushSize: 4,
  brush: "solid",
  color: { r: 255, g: 0, b: 16, a: 255 },
  points: [
    { x: 1, y: 2 },
    { x: 5, y: 2 },
  ],
  mask: { type: 0, r: 0, g: 0, b: 0 },
});

describe("reading a recording as a log", () => {
  it("names the sender and says what the mark was", () => {
    const [row] = logRows([entry(1, encodePainterOperation(1, stroke()))], NAMES);
    expect(row).toMatchObject({ seq: 1, kind: "stroke", actor: "miro", drawable: true, position: 0 });
    expect(row.summary).toBe("fg · solid 4px · #ff0010 · 2 pts");
  });

  /** Drawing on somebody else's layer is rare, and it is the case worth
   * seeing, so it is the only time the layer's owner is spelled out. */
  it("says whose layer a mark went on when it was not the sender's", () => {
    const [row] = logRows([entry(1, encodePainterOperation(1, stroke("2")))], NAMES);
    expect(row.summary.startsWith("oeee's fg")).toBe(true);
  });

  it("falls back to the session id for someone the manifest does not name", () => {
    const [row] = logRows([entry(1, encodePainterOperation(9, stroke()))], NAMES);
    expect(row.actor).toBe("#9");
  });

  /**
   * A row that did not draw leaves the canvas where the last one put it, so
   * choosing it shows the drawing as it stood at that moment rather than
   * jumping to the next mark.
   */
  it("puts a row that did not draw at the canvas the previous mark left", () => {
    const rows = logRows(
      [
        entry(1, encodePainterOperation(1, stroke())),
        entry(2, encodePointerUp(1)),
        entry(3, encodePainterOperation(2, { kind: "undo", redo: false })),
      ],
      NAMES,
    );
    expect(rows.map((row) => [row.drawable, row.position])).toEqual([
      [true, 0],
      [false, 0],
      [true, 1],
    ]);
    expect(rows[2]).toMatchObject({ actor: "oeee", summary: "undo" });
  });

  /** A message type written after this build is still a row, not a hole. */
  it("keeps bytes it cannot read as a row saying so", () => {
    const [row] = logRows([entry(1, Uint8Array.from([0xee, 1, 2]).buffer)], NAMES);
    expect(row).toMatchObject({ kind: "unknown", drawable: false, position: -1 });
    expect(row.summary).toBe("3 bytes, type 0xee");
  });
});

describe("the time column", () => {
  it("reads as minutes, seconds and tenths from the start", () => {
    expect(elapsed(0)).toBe("+0:00.0");
    expect(elapsed(61_250)).toBe("+1:01.2");
    expect(elapsed(3_600_000)).toBe("+60:00.0");
  });
});
