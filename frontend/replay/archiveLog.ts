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

/** `OEEELOG` and a format byte. */
const MAGIC = Uint8Array.from([0x4f, 0x45, 0x45, 0x45, 0x4c, 0x4f, 0x47, 0x01]);

/** One message as it was recorded. */
export type ArchivedEntry = {
  /** Milliseconds since the epoch, stamped by the server when it assigned the
   * position. The drawing messages carry no time of their own, so this is the
   * only thing a replay can pace itself by. */
  at: number;
  /** Canonical position. Contiguous in a whole recording. */
  seq: number;
  /** The connection that sent it -- which the live history does not keep, and
   * which is how a report from one client is matched to its own traffic. */
  from: string;
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

/**
 * The broadcast header: `1|<from>|<target>|<seq>|<history>` and a newline,
 * then the payload untouched.
 */
function decodeFrame(frame: Uint8Array): Omit<ArchivedEntry, "at"> | null {
  const newline = frame.indexOf(0x0a);
  if (newline < 0) return null;
  const header = new TextDecoder().decode(frame.subarray(0, newline));
  const fields = header.split("|");
  if (fields.length !== 5 || fields[0] !== "1") return null;
  const seq = Number(fields[3]);
  if (!Number.isSafeInteger(seq)) return null;
  return {
    seq,
    from: fields[1],
    historyId: fields[4],
    payload: frame.subarray(newline + 1),
  };
}

/**
 * Every whole entry in a recording, in order.
 *
 * `null` only when the file is not a recording at all, so "not ours" is
 * distinguishable from "empty". A truncated tail costs the entries in it and
 * nothing before them -- the file most worth reading is the one from a session
 * that went wrong, which is also the one most likely to be short.
 */
export function decodeArchive(bytes: Uint8Array): ArchivedEntry[] | null {
  if (!startsWithMagic(bytes, 0)) return null;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const entries: ArchivedEntry[] = [];
  let at = MAGIC.length;
  while (at + 12 <= bytes.length) {
    if (startsWithMagic(bytes, at)) {
      at += MAGIC.length;
      continue;
    }
    const length = view.getUint32(at, true);
    const timestamp = Number(view.getBigUint64(at + 4, true));
    const start = at + 12;
    if (start + length > bytes.length) break;
    const frame = decodeFrame(bytes.subarray(start, start + length));
    // Framing we do not recognise: stop rather than guess where the next entry
    // begins.
    if (!frame) break;
    entries.push({ at: timestamp, ...frame });
    at = start + length;
  }
  return entries;
}

/** What a recording says about itself, written beside it. */
export type ArchiveManifest = {
  session: string;
  width: number;
  height: number;
  first_seq: number | null;
  last_seq: number | null;
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
  return manifest.first_seq === 1;
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
