import { afterEach, describe, expect, it, vi } from "vitest";
import { mount, type PainterHandle, type PainterOperation } from "neo-cucumber";
import { encodePainterOperation, encodeUndo, encodeUndoPoint } from "../collaborate/binaryProtocol";
import { decodeArchive, type ArchivedEntry } from "./archiveLog";
import { createReplay, drawableEntries, timeline, trailingCheckpoints, MAX_GAP_MS } from "./player";

/**
 * A replay has to reach the canvas it recorded.
 *
 * Through the real painter, because that is the whole claim: the same
 * `applyCanonicalOperation` a live client applies its room's messages with.
 * Anything that rendered a recording some other way would be a second opinion
 * about what a stroke looks like, and the two would drift.
 */

const WIDTH = 64;
const HEIGHT = 48;
const MAGIC = [0x4f, 0x45, 0x45, 0x45, 0x4c, 0x4f, 0x47, 0x02];
const HISTORY = "00000000-0000-0000-0000-000000000007";

let hosts: HTMLElement[] = [];
let painters: PainterHandle[] = [];

afterEach(() => {
  for (const painter of painters) painter.unmount();
  for (const host of hosts) host.remove();
  painters = [];
  hosts = [];
});

function stroke(at: { x: number; y: number }): PainterOperation {
  return {
    kind: "stroke",
    layer: "foreground",
    brushSize: 4,
    brush: "solid",
    color: { r: 0, g: 0, b: 0, a: 255 },
    points: [
      { x: at.x, y: at.y },
      { x: at.x + 4, y: at.y },
    ],
    mask: { type: 0, r: 0, g: 0, b: 0 },
  };
}

/** A recording, framed exactly as the server writes one. */
function archive(marks: { user: number; at: number; point: { x: number; y: number } }[]): Uint8Array {
  return recording(
    marks.map((mark) => ({
      user: mark.user,
      at: mark.at,
      payload: new Uint8Array(encodePainterOperation(mark.user, stroke(mark.point))),
    })),
  );
}

/** Any messages at all, sequenced from one in the order given. */
function recording(messages: { user: number; at: number; payload: Uint8Array }[]): Uint8Array {
  const senders = [...new Set(messages.map((message) => `conn-${message.user}`))];
  const raw = HISTORY.replace(/-/g, "").match(/../g)!.map((b) => parseInt(b, 16));
  const bytes: number[] = [...MAGIC, ...raw, senders.length];
  for (const sender of senders) {
    const name = [...new TextEncoder().encode(sender)];
    bytes.push(name.length, ...name);
  }
  messages.forEach((message, index) => {
    const fixed = new Uint8Array(22);
    const view = new DataView(fixed.buffer);
    fixed[0] = 0;
    fixed[1] = senders.indexOf(`conn-${message.user}`);
    view.setBigUint64(2, BigInt(index + 1), true);
    view.setBigUint64(10, BigInt(message.at), true);
    view.setUint32(18, message.payload.length, true);
    bytes.push(...fixed, ...message.payload);
  });
  return Uint8Array.from(bytes);
}

/** RESET_POINT as the server sequences it: a base and a snapshot count. */
function resetPoint(base: number): Uint8Array {
  const payload = new Uint8Array(11);
  payload[0] = 0x0d;
  new DataView(payload.buffer).setBigUint64(1, BigInt(base), true);
  payload[9] = 2;
  return payload;
}

async function newPainter(host: HTMLElement): Promise<PainterHandle> {
  // Same order the page uses: release the previous painter before emptying
  // the host it put its nodes in.
  painters.pop()?.unmount();
  host.textContent = "";
  const painter = mount(host, {
    width: WIDTH,
    height: HEIGHT,
    mode: { kind: "standard" },
    controls: { kind: "none" },
    recordReplay: false,
    synchronization: { actorId: "replay-viewer", onOperation: () => {} },
  });
  await painter.ready;
  painter.setInteractionEnabled(false);
  painters.push(painter);
  return painter;
}

/** Waits for the painter to have put its pixels on the DOM canvases, which it
 * does on a frame rather than in the call that drew them. */
async function settle(): Promise<void> {
  for (let index = 0; index < 4; index++) {
    await new Promise((resolve) => requestAnimationFrame(() => resolve(null)));
  }
  await new Promise((resolve) => setTimeout(resolve, 20));
}

/** Opaque pixels around a point, across every mounted layer -- or only the
 * ones on show, for asking what a viewer would see. */
function inkAt(point: { x: number; y: number }, shownOnly = false): boolean {
  for (const canvas of document.querySelectorAll("canvas")) {
    if (canvas.width !== WIDTH || canvas.height !== HEIGHT) continue;
    if (shownOnly && canvas.style.display === "none") continue;
    const context = canvas.getContext("2d");
    if (!context) continue;
    const x0 = Math.max(0, point.x - 4);
    const y0 = Math.max(0, point.y - 4);
    const data = context.getImageData(x0, y0, 14, 14).data;
    for (let i = 3; i < data.length; i += 4) if (data[i] > 0) return true;
  }
  return false;
}

async function harness(marks: Parameters<typeof archive>[0], painters: PainterHandle[] = []) {
  return harnessOf(decodeArchive(archive(marks)) as ArchivedEntry[], painters);
}

async function harnessOf(entries: ArchivedEntry[], painters: PainterHandle[] = []) {
  const host = document.createElement("div");
  document.body.appendChild(host);
  hosts.push(host);
  const painter = await newPainter(host);
  painters.push(painter);
  const replay = createReplay({
    painter,
    entries,
    remount: async () => {
      const next = await newPainter(host);
      painters.push(next);
      return next;
    },
  });
  return { replay, entries };
}

const FIRST = { x: 8, y: 8 };
const SECOND = { x: 40, y: 30 };

describe("replaying a recording", () => {
  it("reaches the canvas the recording describes", async () => {
    const { replay } = await harness([
      { user: 1, at: 1000, point: FIRST },
      { user: 2, at: 1200, point: SECOND },
    ]);

    expect(replay.length).toBe(2);
    await settle();
    expect(inkAt(FIRST)).toBe(false);

    await replay.seek(replay.length - 1);
    await settle();
    expect(inkAt(FIRST)).toBe(true);
    // Each participant's mark lands, not only the first one's -- a session has
    // a layer pair per person and a viewer that composited one would look
    // right until two people drew.
    expect(inkAt(SECOND)).toBe(true);
  });

  it("applies one message at a time on the way there", async () => {
    const { replay } = await harness([
      { user: 1, at: 1000, point: FIRST },
      { user: 2, at: 1200, point: SECOND },
    ]);

    await replay.seek(0);
    await settle();
    expect(inkAt(FIRST)).toBe(true);
    expect(inkAt(SECOND)).toBe(false);
  });

  /**
   * Going back means starting over: applying operations is the only way
   * pixels get onto this canvas, so there is nothing to undo them with.
   */
  it("rebuilds from blank when seeking backwards", async () => {
    const { replay } = await harness([
      { user: 1, at: 1000, point: FIRST },
      { user: 2, at: 1200, point: SECOND },
    ]);

    await replay.seek(replay.length - 1);
    await settle();
    expect(inkAt(SECOND)).toBe(true);

    await replay.seek(0);
    await settle();
    expect(inkAt(FIRST)).toBe(true);
    expect(inkAt(SECOND)).toBe(false);

    await replay.seek(-1);
    await settle();
    expect(inkAt(FIRST)).toBe(false);
  });

  /**
   * A live session grows while it is watched. What arrives later has to land
   * exactly as it would have had it been there from the start.
   */
  it("takes messages recorded since and plays them after the rest", async () => {
    const whole = decodeArchive(
      archive([
        { user: 1, at: 1000, point: FIRST },
        { user: 2, at: 1200, point: SECOND },
      ]),
    ) as ArchivedEntry[];
    const { replay } = await harness([{ user: 1, at: 1000, point: FIRST }]);
    await replay.seek(replay.length - 1);
    expect(replay.length).toBe(1);

    replay.append(whole.slice(1));
    expect(replay.length).toBe(2);
    await settle();
    // Appending moves nothing by itself.
    expect(inkAt(SECOND)).toBe(false);

    await replay.seek(replay.length - 1);
    await settle();
    expect(inkAt(FIRST)).toBe(true);
    expect(inkAt(SECOND)).toBe(true);
  });

  /** Looking at one person's marks alone is how "my strokes vanished" gets
   * answered. Hiding is a view: the pixels are still there. */
  it("hides a participant's layers and shows them again", async () => {
    const painters: PainterHandle[] = [];
    const { replay } = await harness(
      [
        { user: 1, at: 1000, point: FIRST },
        { user: 2, at: 1200, point: SECOND },
      ],
      painters,
    );
    await replay.seek(replay.length - 1);
    const painter = painters[painters.length - 1];

    painter.setHiddenParticipants(["2"]);
    await settle();
    expect(inkAt(FIRST, true)).toBe(true);
    expect(inkAt(SECOND, true)).toBe(false);
    expect(inkAt(SECOND)).toBe(true);

    painter.setHiddenParticipants([]);
    await settle();
    expect(inkAt(SECOND, true)).toBe(true);
  });

  it("plays forward on its own and stops at the end", async () => {
    const { replay } = await harness([
      { user: 1, at: 1000, point: FIRST },
      { user: 2, at: 1010, point: SECOND },
    ]);

    replay.play();
    await vi.waitFor(() => expect(inkAt(SECOND)).toBe(true), { timeout: 5000 });
    replay.destroy();
  });
});

describe("pacing", () => {
  const entries = (times: number[]): ArchivedEntry[] =>
    times.map((at, index) => ({
      at,
      seq: index + 1,
      from: "conn",
      historyId: HISTORY,
      payload: new Uint8Array(),
    }));

  /** Gaps within a stroke are milliseconds and survive untouched. */
  it("keeps the real gaps between messages", () => {
    expect(timeline(entries([1000, 1040, 1060]))).toEqual([0, 40, 60]);
  });

  /** A room spends most of its life idle, and watching that back faithfully
   * would mean watching nothing. */
  it("shortens the pauses between them", () => {
    expect(timeline(entries([0, 600_000, 600_010]))).toEqual([
      0,
      MAX_GAP_MS,
      MAX_GAP_MS + 10,
    ]);
  });

  /**
   * Recorded times are server arrival times and repeat: a burst sequenced
   * inside one millisecond is due all at once, because that is what it was.
   * Spacing them out is what made playback stutter -- a timer per message
   * cannot go below the browser's clamp, so eight hundred messages recorded
   * at the same instant took three and a half seconds to replay.
   */
  it("makes a burst due all at once", () => {
    expect(timeline(entries([1000, 1000, 1000, 1000]))).toEqual([0, 0, 0, 0]);
  });

  it("starts at nothing", () => {
    expect(timeline(entries([1000]))).toEqual([0]);
    expect(timeline([])).toEqual([]);
  });
});

describe("what a replay draws", () => {
  /**
   * RESET_POINT tells a live client that history below a base was squashed
   * into snapshots it is about to receive. A recording has no snapshots
   * because it kept the operations instead, so the marker is nothing to draw
   * and applying it as one would be applying a message that is not a mark.
   * It is kept on the mark after it, which compacts at it first.
   */
  it("carries a checkpoint on the mark after it rather than drawing it", () => {
    const drawing = new Uint8Array(encodePainterOperation(1, stroke(FIRST)));
    const entries: ArchivedEntry[] = [
      { at: 1, seq: 1, from: "c", historyId: HISTORY, payload: resetPoint(0) },
      { at: 2, seq: 2, from: "c", historyId: HISTORY, payload: drawing },
      { at: 3, seq: 3, from: "c", historyId: HISTORY, payload: resetPoint(2) },
    ];
    const drawable = drawableEntries(entries);
    expect(drawable).toHaveLength(1);
    expect(drawable[0].entry.seq).toBe(2);
    expect(drawable[0].checkpoints).toEqual([0]);
    expect(trailingCheckpoints(entries)).toEqual([2]);
  });
});

/**
 * An undo cannot reach under a checkpoint.
 *
 * Every client in the room compacts at a RESET_POINT, and what is compacted
 * is frozen: an undo sent after it -- clicked before its sender's own client
 * had compacted, say -- finds nothing of theirs above the base and does
 * nothing. Session 486a0954 at 15:02:38 is this exactly: babguette drew, a
 * checkpoint landed at the end of the stroke, and the two undos that followed
 * left both strokes on everybody's canvas. The replay, which skipped the
 * checkpoint, took them both back.
 */
describe("undo across a checkpoint", () => {
  const undoPoint = () => new Uint8Array(encodeUndoPoint(1));
  const undo = () => new Uint8Array(encodeUndo(1, false));
  const mark = (point: { x: number; y: number }) =>
    new Uint8Array(encodePainterOperation(1, stroke(point)));
  const THIRD = { x: 24, y: 36 };

  // seq 1-4: two strokes. seq 5: the checkpoint at the end of them.
  const frozen = [
    { user: 1, at: 1000, payload: undoPoint() },
    { user: 1, at: 1001, payload: mark(FIRST) },
    { user: 1, at: 1100, payload: undoPoint() },
    { user: 1, at: 1101, payload: mark(SECOND) },
    { user: 1, at: 1200, payload: resetPoint(4) },
  ];

  it("leaves what the checkpoint froze where the room left it", async () => {
    const { replay } = await harnessOf(
      decodeArchive(
        recording([
          ...frozen,
          { user: 1, at: 1300, payload: undo() },
          { user: 1, at: 1301, payload: undo() },
        ]),
      ) as ArchivedEntry[],
    );
    await replay.seek(replay.length - 1);
    await settle();
    expect(inkAt(FIRST)).toBe(true);
    expect(inkAt(SECOND)).toBe(true);
  });

  it("still undoes what was drawn after it", async () => {
    const { replay } = await harnessOf(
      decodeArchive(
        recording([
          ...frozen,
          { user: 1, at: 1300, payload: undoPoint() },
          { user: 1, at: 1301, payload: mark(THIRD) },
          { user: 1, at: 1400, payload: undo() },
          // Nothing of theirs above the base is left to take.
          { user: 1, at: 1401, payload: undo() },
        ]),
      ) as ArchivedEntry[],
    );
    await replay.seek(replay.length - 1);
    await settle();
    expect(inkAt(THIRD)).toBe(false);
    expect(inkAt(FIRST)).toBe(true);
    expect(inkAt(SECOND)).toBe(true);
  });

  /** A live session's checkpoint can be the last thing in one poll and the
   * undo the first thing in the next. */
  it("holds when the checkpoint and the undo arrive in different appends", async () => {
    const whole = decodeArchive(
      recording([...frozen, { user: 1, at: 1300, payload: undo() }]),
    ) as ArchivedEntry[];
    const { replay } = await harnessOf(whole.slice(0, frozen.length));
    replay.append(whole.slice(frozen.length));
    await replay.seek(replay.length - 1);
    await settle();
    expect(inkAt(FIRST)).toBe(true);
    expect(inkAt(SECOND)).toBe(true);
  });

  /** Seeking back starts over from blank, and the checkpoint has to be
   * applied again on the way forward. */
  it("holds after seeking back and forward again", async () => {
    const { replay } = await harnessOf(
      decodeArchive(
        recording([...frozen, { user: 1, at: 1300, payload: undo() }]),
      ) as ArchivedEntry[],
    );
    await replay.seek(replay.length - 1);
    await replay.seek(0);
    await replay.seek(replay.length - 1);
    await settle();
    expect(inkAt(FIRST)).toBe(true);
    expect(inkAt(SECOND)).toBe(true);
  });
});
