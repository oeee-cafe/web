import { describe, expect, it } from "vitest";
import type { LogRow } from "./logRows";
import { markers, QUIET_MS } from "./markers";

const row = (seq: number, at: number, kind: string, position: number, drawable = true): LogRow => ({
  index: seq - 1,
  seq,
  at,
  kind,
  actor: "miro",
  sessionId: 1,
  target: null,
  onOther: false,
  summary: "",
  drawable,
  position,
});

describe("moments on the scrubber", () => {
  const drawTimes = [1_000, 2_000, 2_000 + QUIET_MS + 5_000, 70_000];
  const rows = [
    row(1, 1_000, "stroke", 0),
    row(2, 2_000, "stroke", 1),
    row(3, 3_000, "resetPoint", 1, false),
    row(4, drawTimes[2], "stroke", 2),
  ];

  it("puts a line of chat where the canvas stood when it was said", () => {
    const found = markers({
      rows,
      drawTimes,
      chat: [{ at: 2_500, user_id: "u", login_name: "miro", message: "hi" }],
      reports: [],
    });
    expect(found).toContainEqual({ kind: "chat", position: 1, label: "miro: hi" });
  });

  it("marks reports and checkpoints", () => {
    const found = markers({ rows, drawTimes, chat: [], reports: [{ at: 1_500, label: "oeee: canonical-gap" }] });
    expect(found).toContainEqual({ kind: "report", position: 0, label: "oeee: canonical-gap" });
    expect(found).toContainEqual({ kind: "checkpoint", position: 1, label: "checkpoint, seq 3" });
  });

  /** Placed at the last mark before the pause, which is the canvas as it
   * stood through it. */
  it("marks a minute or more with nothing drawn, and nothing shorter", () => {
    const quiet = markers({ rows, drawTimes, chat: [], reports: [] }).filter((m) => m.kind === "quiet");
    expect(quiet).toEqual([{ kind: "quiet", position: 1, label: "quiet for 1m 5s" }]);
  });
});
