import { describe, expect, it } from "vitest";
import { decodeArchive, isRenderable, type ArchiveManifest } from "./archiveLog";

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

const MAGIC = [0x4f, 0x45, 0x45, 0x45, 0x4c, 0x4f, 0x47, 0x01];

function entry(at: number, seq: number, from: string, payload: number[]): number[] {
  const header = new TextEncoder().encode(`1|${from}||${seq}|history-id\n`);
  const frame = [...header, ...payload];
  const prefix = new Uint8Array(12);
  const view = new DataView(prefix.buffer);
  view.setUint32(0, frame.length, true);
  view.setBigUint64(4, BigInt(at), true);
  return [...prefix, ...frame];
}

function chunk(...entries: number[][]): Uint8Array {
  return Uint8Array.from([...MAGIC, ...entries.flat()]);
}

describe("reading a recorded session", () => {
  it("reads every entry with its position, sender and time", () => {
    const decoded = decodeArchive(
      chunk(
        entry(1_700_000_000_000, 1, "conn-a", [0x16, 0x01]),
        entry(1_700_000_000_120, 2, "conn-b", [0x14, 0x02]),
      ),
    );
    expect(decoded).toHaveLength(2);
    expect(decoded![0]).toMatchObject({ at: 1_700_000_000_000, seq: 1, from: "conn-a" });
    expect([...decoded![0].payload]).toEqual([0x16, 0x01]);
    // Which connection sent it is the attribution the live history drops.
    expect(decoded![1].from).toBe("conn-b");
    expect(decoded![1].seq).toBe(2);
  });

  /** Payloads are arbitrary bytes; the length prefix is what keeps a
   * delimiter out of the question. */
  it("reads a payload that looks like framing", () => {
    const payload = [...MAGIC, 0x0a, 0x7c, 0x31, 0x7c];
    const decoded = decodeArchive(chunk(entry(5, 9, "conn-a", payload)));
    expect([...decoded![0].payload]).toEqual(payload);
  });

  /** A session is downloaded as its objects end to end. */
  it("reads concatenated chunks as one log", () => {
    const joined = Uint8Array.from([
      ...chunk(entry(1, 1, "conn-a", [0x16])),
      ...chunk(entry(2, 2, "conn-a", [0x17])),
    ]);
    expect(decodeArchive(joined)!.map((held) => held.seq)).toEqual([1, 2]);
  });

  it("is empty rather than absent for a recording with nothing in it", () => {
    expect(decodeArchive(chunk())).toEqual([]);
  });

  it("refuses a file that is not a recording", () => {
    expect(decodeArchive(new Uint8Array())).toBeNull();
    expect(decodeArchive(Uint8Array.from([1, 2, 3, 4, 5, 6, 7, 8]))).toBeNull();
  });

  /** A flush that died, or a download that stopped: everything whole before
   * the cut still reads, which is the point of a per-entry length. */
  it("reads up to a truncated tail", () => {
    const whole = chunk(
      entry(1, 1, "conn-a", [0x16, 0x01]),
      entry(2, 2, "conn-a", [0x16, 0x02]),
      entry(3, 3, "conn-a", [0x16, 0x03]),
    );
    for (let cut = MAGIC.length; cut < whole.length; cut++) {
      const decoded = decodeArchive(whole.subarray(0, cut));
      expect(decoded).not.toBeNull();
      const seqs = decoded!.map((held) => held.seq);
      expect(seqs).toEqual([1, 2, 3].slice(0, seqs.length));
    }
  });
});

describe("whether a recording can be rendered", () => {
  const manifest = (first: number | null): ArchiveManifest => ({
    session: "s",
    width: 300,
    height: 300,
    first_seq: first,
    last_seq: 10,
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
