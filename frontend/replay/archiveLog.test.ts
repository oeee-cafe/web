import { describe, expect, it } from "vitest";
import {
  decodeArchive,
  isRenderable,
  positionAt,
  type ArchiveManifest,
} from "./archiveLog";

/**
 * The reader and the writer are in different languages, so the only thing
 * holding them together is that both are written from the same description of
 * the format. These are the cases where they could drift apart without either
 * side noticing: the length prefix, the header fields, and what a short file
 * does.
 *
 * `a_chunk_round_trips_every_entry_in_order` and its neighbours in
 * `collaborate::archive` are the other half.
 */

const MAGIC = [0x4f, 0x45, 0x45, 0x45, 0x4c, 0x4f, 0x47, 0x02];
const HISTORY = "00000000-0000-0000-0000-000000000007";

/** A chunk header: history, then the senders named once. */
function header(senders: string[], history = HISTORY): number[] {
  const raw = history.replace(/-/g, "").match(/../g)!.map((b) => parseInt(b, 16));
  const out = [...MAGIC, ...raw, senders.length];
  for (const sender of senders) {
    const bytes = [...new TextEncoder().encode(sender)];
    out.push(bytes.length, ...bytes);
  }
  return out;
}

function entry(
  at: number,
  seq: number,
  sender: number,
  payload: number[],
  kind = 0,
): number[] {
  const fixed = new Uint8Array(22);
  const view = new DataView(fixed.buffer);
  fixed[0] = kind;
  fixed[1] = sender;
  view.setBigUint64(2, BigInt(seq), true);
  view.setBigUint64(10, BigInt(at), true);
  view.setUint32(18, payload.length, true);
  return [...fixed, ...payload];
}

function chunk(senders: string[], ...entries: number[][]): Uint8Array {
  return Uint8Array.from([...header(senders), ...entries.flat()]);
}

describe("reading a recorded session", () => {
  it("reads every entry with its position, sender and time", () => {
    const decoded = decodeArchive(
      chunk(
        ["conn-a", "conn-b"],
        entry(1_700_000_000_000, 1, 0, [0x16, 0x01]),
        entry(1_700_000_000_120, 2, 1, [0x14, 0x02]),
      ),
    );
    expect(decoded).toHaveLength(2);
    expect(decoded![0]).toMatchObject({ at: 1_700_000_000_000, seq: 1, from: "conn-a" });
    expect([...decoded![0].payload]).toEqual([0x16, 0x01]);
    // Which connection sent it is the attribution the live history drops,
    // named once in the header rather than repeated per message.
    expect(decoded![1].from).toBe("conn-b");
    expect(decoded![1].historyId).toBe(HISTORY);
  });

  /** Payloads are arbitrary bytes; the length prefix is what keeps a
   * delimiter out of the question. */
  it("reads a payload that looks like framing", () => {
    const payload = [...MAGIC, 0x0a, 0x7c, 0x31, 0x7c];
    const decoded = decodeArchive(chunk(["conn-a"], entry(5, 9, 0, payload)));
    expect([...decoded![0].payload]).toEqual(payload);
  });

  /** A session is downloaded as its objects end to end, and the second
   * chunk's own header governs what follows it. */
  it("reads concatenated chunks as one log", () => {
    const other = "00000000-0000-0000-0000-00000000000b";
    const joined = Uint8Array.from([
      ...chunk(["conn-a"], entry(1, 1, 0, [0x16])),
      ...header(["conn-z"], other),
      ...entry(2, 2, 0, [0x17]),
    ]);
    const decoded = decodeArchive(joined)!;
    expect(decoded.map((held) => held.seq)).toEqual([1, 2]);
    expect(decoded.map((held) => held.from)).toEqual(["conn-a", "conn-z"]);
    expect(decoded.map((held) => held.historyId)).toEqual([HISTORY, other]);
  });

  it("is empty rather than absent for a recording with nothing in it", () => {
    expect(decodeArchive(chunk([]))).toEqual([]);
  });

  /**
   * The byte that lets this format grow. An entry of a kind written after
   * this reader is skipped by its length, so a file with one in it still
   * yields everything else.
   */
  it("skips an entry of a kind it does not know", () => {
    const decoded = decodeArchive(
      chunk(
        ["conn-a"],
        entry(1, 1, 0, [0x16]),
        entry(2, 2, 0, [0xff, 0xff], 99),
        entry(3, 3, 0, [0x17]),
      ),
    );
    expect(decoded!.map((held) => held.seq)).toEqual([1, 3]);
  });

  it("refuses a file that is not a recording", () => {
    expect(decodeArchive(new Uint8Array())).toBeNull();
    expect(decodeArchive(Uint8Array.from([1, 2, 3, 4, 5, 6, 7, 8]))).toBeNull();
    // The previous format, which this one replaced.
    expect(decodeArchive(Uint8Array.from([0x4f, 0x45, 0x45, 0x45, 0x4c, 0x4f, 0x47, 0x01]))).toBeNull();
  });

  /** A flush that died, or a download that stopped: everything whole before
   * the cut still reads, which is the point of a per-entry length. */
  it("reads up to a truncated tail", () => {
    const whole = chunk(
      ["conn-a"],
      entry(1, 1, 0, [0x16, 0x01]),
      entry(2, 2, 0, [0x16, 0x02]),
      entry(3, 3, 0, [0x16, 0x03]),
    );
    for (let cut = header(["conn-a"]).length; cut < whole.length; cut++) {
      const decoded = decodeArchive(whole.subarray(0, cut));
      expect(decoded).not.toBeNull();
      const seqs = decoded!.map((held) => held.seq);
      expect(seqs).toEqual([1, 2, 3].slice(0, seqs.length));
    }
  });
});

describe("whether a recording can be rendered", () => {
  const manifest = (first: number | null): ArchiveManifest => ({
    format: "oeee-collab-archive",
    version: 2,
    session: "s",
    canvas: { width: 300, height: 300, mode: "standard" },
    started_at: "2026-08-21T00:00:00Z",
    ended_at: null,
    duration_ms: null,
    recording: { first_seq: first, last_seq: 10, first_at: 1, last_at: 2, messages: 10 },
    participants: [],
    sealed: true,
  });

  /**
   * Checkpoint snapshots are not archived, so only a log that starts at the
   * room's first message can produce the drawing. One that starts later is
   * missing everything a checkpoint squashed, and drawing it anyway would
   * present a fragment as the finished picture.
   */
  it("needs the recording to start at the room's first message", () => {
    expect(isRenderable(manifest(1))).toBe(true);
    expect(isRenderable(manifest(3310))).toBe(false);
    expect(isRenderable(manifest(null))).toBe(false);
  });
});

describe("placing a moment against a recording", () => {
  const times = [1_000, 1_000, 4_500, 300_000];

  it("finds the last message sequenced at or before it", () => {
    expect(positionAt(times, 4_500)).toBe(2);
    expect(positionAt(times, 4_499)).toBe(1);
    // A burst inside one millisecond is all there at once.
    expect(positionAt(times, 1_000)).toBe(1);
  });

  /**
   * By the clock, not by the player's schedule. The schedule caps a pause at a
   * second or so, and a line said just before the drawing resumed after five
   * minutes' quiet belongs before the stroke that ended the quiet.
   */
  it("keeps a long pause as long as it was", () => {
    expect(positionAt(times, 299_999)).toBe(2);
  });

  /** Chat carries the sender's clock, so a line can be stamped before the
   * first message; it belongs on the blank canvas. */
  it("puts a moment before the recording on the blank canvas", () => {
    expect(positionAt(times, 500)).toBe(-1);
    expect(positionAt([], 500)).toBe(-1);
  });

  it("puts a moment after the recording on the finished canvas", () => {
    expect(positionAt(times, 900_000)).toBe(3);
  });
});
