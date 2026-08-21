/**
 * Reading a recorded session back.
 *
 * The mirror of `encode_chunk` in `collaborate::archive`: the magic, then each
 * entry as a length, the moment it was sequenced, and the broadcast frame the
 * room saw. Chunks concatenate, so a whole session downloaded as one stream
 * reads here exactly like one of the objects it is made of.
 *
 * Deliberately separate from applying any of it. What a message means is
 * `binaryProtocol`'s business and what it does to a canvas is the painter's;
 * this only says what was in the file and in what order.
 */

/** `OEEELOG` and the format version. */
const MAGIC = Uint8Array.from([0x4f, 0x45, 0x45, 0x45, 0x4c, 0x4f, 0x47, 0x02]);

/** Everything today is a canonical message. A kind written after this reader
 * is skipped by its length rather than losing the file. */
const KIND_MESSAGE = 0;

/** kind, sender index, sequence, time, length. */
const ENTRY_HEADER = 1 + 1 + 8 + 8 + 4;

/** One message as it was recorded. */
export type ArchivedEntry = {
  /** Milliseconds since the epoch, stamped by the server when it assigned the
   * position. Drawing messages carry no time of their own, so this is the only
   * thing a replay can pace itself by. */
  at: number;
  /** Canonical position. Contiguous in a whole recording. */
  seq: number;
  /** The connection that sent it, or `system` for what the server sequenced on
   * the room's behalf. Named once in the chunk header and pointed at here. */
  from: string;
  /** Which history this position belongs to, so a replaced history is visible
   * without tracking chunks. */
  historyId: string;
  /** The message itself, exactly as the room received it. */
  payload: Uint8Array;
};

function startsWithMagic(bytes: Uint8Array, at: number): boolean {
  if (at + MAGIC.length > bytes.length) return false;
  for (let index = 0; index < MAGIC.length; index++) {
    if (bytes[at + index] !== MAGIC[index]) return false;
  }
  return true;
}

function uuidFrom(bytes: Uint8Array): string {
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/**
 * A chunk header: what is true of everything after it.
 *
 * ```text
 * "OEEELOG\x02" | history_id (16) | senders (u8 count, each u8 len + utf8)
 * ```
 */
function readHeader(
  bytes: Uint8Array,
  at: number,
): { historyId: string; senders: string[]; next: number } | null {
  let cursor = at + MAGIC.length;
  if (cursor + 17 > bytes.length) return null;
  const historyId = uuidFrom(bytes.subarray(cursor, cursor + 16));
  cursor += 16;
  const count = bytes[cursor];
  cursor += 1;
  const senders: string[] = [];
  const text = new TextDecoder();
  for (let index = 0; index < count; index++) {
    if (cursor >= bytes.length) return null;
    const length = bytes[cursor];
    cursor += 1;
    if (cursor + length > bytes.length) return null;
    senders.push(text.decode(bytes.subarray(cursor, cursor + length)));
    cursor += length;
  }
  return { historyId, senders, next: cursor };
}

/**
 * Every whole entry in a recording, in order.
 *
 * `null` only when the file does not begin with a header, so "not ours" is
 * distinguishable from "empty". A truncated tail costs the entries in it and
 * nothing before them -- the file most worth reading is the one from a session
 * that went wrong, which is also the one most likely to be short.
 *
 * Chunks concatenate: a header met at an entry boundary replaces what is true
 * of the entries after it, so a whole session downloaded end to end reads
 * exactly like one of the objects it is made of.
 */
export function decodeArchive(bytes: Uint8Array): ArchivedEntry[] | null {
  if (!startsWithMagic(bytes, 0)) return null;
  let header = readHeader(bytes, 0);
  if (!header) return null;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const entries: ArchivedEntry[] = [];
  let at = header.next;
  while (at + ENTRY_HEADER <= bytes.length) {
    if (startsWithMagic(bytes, at)) {
      const next = readHeader(bytes, at);
      if (!next) break;
      header = next;
      at = next.next;
      continue;
    }
    const kind = bytes[at];
    const sender = bytes[at + 1];
    const seq = Number(view.getBigUint64(at + 2, true));
    const stamp = Number(view.getBigUint64(at + 10, true));
    const length = view.getUint32(at + 18, true);
    const start = at + ENTRY_HEADER;
    if (start + length > bytes.length) break;
    const payload = bytes.subarray(start, start + length);
    at = start + length;
    if (kind !== KIND_MESSAGE) continue;
    entries.push({
      at: stamp,
      seq,
      from: header.senders[sender] ?? "",
      historyId: header.historyId,
      payload,
    });
  }
  return entries;
}

/** What a recording says about itself, written beside it. */
export type ArchiveManifest = {
  format: string;
  version: number;
  session: string;
  canvas: { width: number; height: number; mode: string };
  started_at: string;
  ended_at: string | null;
  duration_ms: number | null;
  /** What the stored chunks hold, as opposed to what the room reached. */
  recording: {
    first_seq: number | null;
    last_seq: number | null;
    first_at: number | null;
    last_at: number | null;
    messages: number;
  };
  participants: { session_id: number; user_id: string; login_name: string }[];
  sealed: boolean;
};

/**
 * Whether a recording can be rendered from nothing.
 *
 * Checkpoint snapshots are not archived -- a log that starts at the room's
 * first message can produce the same pixels from the operations, and one that
 * does not cannot produce them at all. A session already under way when
 * recording was switched on is the ordinary way this happens, and a viewer has
 * to say so rather than draw a partial canvas as if it were the drawing.
 */
export function isRenderable(manifest: ArchiveManifest): boolean {
  return manifest.recording.first_seq === 1;
}

/** One line of a session's conversation, as it is kept beside the log. */
export type ArchivedChat = {
  at: number;
  user_id: string;
  login_name: string;
  message: string;
};

/**
 * Where each line of the transcript falls on the recording's timeline.
 *
 * Chat carries the sender's own clock and the log carries the server's, so the
 * two are not the same measurement -- this lines them up by wall-clock time
 * against the first recorded message, which is close enough to read a
 * conversation against a drawing and is not offered as anything more.
 */
export function chatOffsets(chat: ArchivedChat[], firstMessageAt: number): number[] {
  return chat.map((line) => Math.max(0, line.at - firstMessageAt));
}
