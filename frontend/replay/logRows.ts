/**
 * The recording as something a person can read: one line per message, saying
 * who did what.
 *
 * Only describes. What a message does to a canvas is the painter's business,
 * reached through `player`; this exists so that somebody looking for the
 * moment a drawing went wrong can find it by reading rather than by scrubbing.
 */

import { decodeMessage, type DecodedMessage } from "../collaborate/binaryProtocol";
import type { ArchivedEntry } from "./archiveLog";
import { toCanonicalOperation } from "./player";

export type LogRow = {
  /** Into the recording's entries. */
  index: number;
  seq: number;
  at: number;
  /** The message type, or `unknown` for bytes this build cannot read. */
  kind: string;
  /** Who sent it, by login name where the manifest says. */
  actor: string;
  /** The session id it was sent under, for messages that carry one. */
  sessionId: number | null;
  /** Whose layers a mark went on, by session id; null for anything that is
   * not a mark on a layer. */
  target: number | null;
  /** A mark made on somebody else's layers. */
  onOther: boolean;
  summary: string;
  /** Whether it put anything on a canvas. The rest -- checkpoints, pointers
   * lifted -- are part of the record and not of the drawing. */
  drawable: boolean;
  /** Where the replay stands once this row has happened: the index of the
   * last drawing message at or before it, -1 for a blank canvas. */
  position: number;
};

type Color = { r: number; g: number; b: number; a: number };

function hex(color: Color): string {
  const channel = (value: number) => value.toString(16).padStart(2, "0");
  const rgb = `#${channel(color.r)}${channel(color.g)}${channel(color.b)}`;
  return color.a === 255 ? rgb : `${rgb} α${color.a}`;
}

function point(at: { x: number; y: number }): string {
  return `${at.x},${at.y}`;
}

/** Names a session id the way the room did. */
function nameOf(names: Map<number, string>, id: number): string {
  return names.get(id) ?? `#${id}`;
}

/**
 * Whose layer a drawing went on, said only when it was not the sender's own --
 * which is rare, and is exactly the case worth seeing.
 */
function onLayer(
  names: Map<number, string>,
  message: { userId: number; targetOwner: number; layer: string },
): string {
  const layer = message.layer === "foreground" ? "fg" : "bg";
  return message.targetOwner === message.userId
    ? layer
    : `${nameOf(names, message.targetOwner)}'s ${layer}`;
}

function actorOf(names: Map<number, string>, message: DecodedMessage): string {
  if ("userId" in message) {
    if (typeof message.userId === "number") return nameOf(names, message.userId);
    if ("username" in message) return message.username;
  }
  return "server";
}

function summarize(names: Map<number, string>, message: DecodedMessage): string {
  switch (message.type) {
    case "stroke":
      return `${onLayer(names, message)} · ${message.brushType} ${message.brushSize}px · ${hex(message.color)} · ${message.points.length} pts`;
    case "line":
      return `${onLayer(names, message)} · ${message.brushType} ${message.brushSize}px · ${hex(message.color)} · ${point(message.from)} → ${point(message.to)}`;
    case "bezier":
      return `${onLayer(names, message)} · ${message.brushType} ${message.brushSize}px · ${hex(message.color)}`;
    case "fill":
      return `${onLayer(names, message)} · ${hex(message.color)} at ${point(message)}`;
    case "region":
      return `${onLayer(names, message)} · ${message.tool} ${message.rect.width}×${message.rect.height} at ${point(message.rect)} · ${hex(message.color)}`;
    case "text":
      return `${onLayer(names, message)} · ${JSON.stringify(message.text)} at ${point(message)}`;
    case "putImage":
      return `${onLayer(names, message)} · ${message.width}×${message.height} at ${point(message)}`;
    case "eraseAll":
      return `${onLayer(names, message)} cleared`;
    case "undo":
    case "undoPoint":
      return "";
    case "resetPoint":
      return `checkpoint at seq ${message.baseSeq}, ${message.snapshotCount} snapshots`;
    default:
      return "";
  }
}

/**
 * Every entry in the recording as a readable row, in order.
 *
 * `names` maps the one-byte session id the stream addresses people by to the
 * login name the manifest recorded for it.
 */
export function logRows(entries: ArchivedEntry[], names: Map<number, string>): LogRow[] {
  let position = -1;
  return entries.map((entry, index) => {
    const message = decodeMessage(entry.payload.slice().buffer as ArrayBuffer);
    const drawable = toCanonicalOperation(entry) !== null;
    if (drawable) position++;
    if (!message) {
      return {
        index,
        seq: entry.seq,
        at: entry.at,
        kind: "unknown",
        actor: entry.from,
        sessionId: null,
        target: null,
        onOther: false,
        summary: `${entry.payload.length} bytes, type 0x${(entry.payload[0] ?? 0).toString(16).padStart(2, "0")}`,
        drawable,
        position,
      };
    }
    return {
      index,
      seq: entry.seq,
      at: entry.at,
      // A redo is the same message as an undo with a flag on it, and reads
      // as the opposite act, so it is its own kind here.
      kind: message.type === "undo" && message.redo ? "redo" : message.type,
      actor: actorOf(names, message),
      sessionId: "userId" in message && typeof message.userId === "number" ? message.userId : null,
      target: "targetOwner" in message ? message.targetOwner : null,
      onOther:
        "targetOwner" in message &&
        typeof message.userId === "number" &&
        message.targetOwner !== message.userId,
      summary: summarize(names, message),
      drawable,
      position,
    };
  });
}

/** `+m:ss.s` from the start of the recording: short enough for a column, and
 * the absolute moment is on the row's title for anyone who needs it. */
export function elapsed(ms: number): string {
  const tenths = Math.max(0, Math.floor(ms / 100));
  const minutes = Math.floor(tenths / 600);
  const seconds = Math.floor((tenths % 600) / 10);
  return `+${minutes}:${String(seconds).padStart(2, "0")}.${tenths % 10}`;
}
