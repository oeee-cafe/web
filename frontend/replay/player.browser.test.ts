import { afterEach, describe, expect, it, vi } from "vitest";
import { mount, type PainterHandle, type PainterOperation } from "neo-cucumber";
import { encodePainterOperation } from "../collaborate/binaryProtocol";
import { decodeArchive, type ArchivedEntry } from "./archiveLog";
import { createReplay, drawableEntries, timeline, MAX_GAP_MS } from "./player";

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
const MAGIC = [0x4f, 0x45, 0x45, 0x45, 0x4c, 0x4f, 0x47, 0x01];

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
  const bytes: number[] = [...MAGIC];
  marks.forEach((mark, index) => {
    const payload = new Uint8Array(encodePainterOperation(mark.user, stroke(mark.point)));
    const header = new TextEncoder().encode(`1|conn-${mark.user}||${index + 1}|history-id\n`);
    const frame = [...header, ...payload];
    const prefix = new Uint8Array(12);
    const view = new DataView(prefix.buffer);
    view.setUint32(0, frame.length, true);
    view.setBigUint64(4, BigInt(mark.at), true);
    bytes.push(...prefix, ...frame);
  });
  return Uint8Array.from(bytes);
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

/** Opaque pixels around a point, across every mounted layer. */
function inkAt(point: { x: number; y: number }): boolean {
  for (const canvas of document.querySelectorAll("canvas")) {
    if (canvas.width !== WIDTH || canvas.height !== HEIGHT) continue;
    const context = canvas.getContext("2d");
    if (!context) continue;
    const x0 = Math.max(0, point.x - 4);
    const y0 = Math.max(0, point.y - 4);
    const data = context.getImageData(x0, y0, 14, 14).data;
    for (let i = 3; i < data.length; i += 4) if (data[i] > 0) return true;
  }
  return false;
}

async function harness(marks: Parameters<typeof archive>[0]) {
  const host = document.createElement("div");
  document.body.appendChild(host);
  hosts.push(host);
  const entries = decodeArchive(archive(marks)) as ArchivedEntry[];
  const painter = await newPainter(host);
  const replay = createReplay({
    painter,
    entries,
    remount: () => newPainter(host),
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
      historyId: "h",
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
   */
  it("skips the messages that were never marks", () => {
    const resetPoint = new Uint8Array(11);
    resetPoint[0] = 0x0d;
    const drawing = new Uint8Array(encodePainterOperation(1, stroke(FIRST)));
    const entries: ArchivedEntry[] = [
      { at: 1, seq: 1, from: "c", historyId: "h", payload: resetPoint },
      { at: 2, seq: 2, from: "c", historyId: "h", payload: drawing },
    ];
    const drawable = drawableEntries(entries);
    expect(drawable).toHaveLength(1);
    expect(drawable[0].entry.seq).toBe(2);
  });
});
