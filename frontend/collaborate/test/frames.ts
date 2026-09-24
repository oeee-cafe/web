import { encodePainterOperation, MSG_TYPE } from "../binaryProtocol";
import { deflateZlib, type PainterOperation } from "neo-cucumber";

/**
 * Frames as the server sends them, built by hand from the wire format in
 * binaryProtocol.ts. Nothing here needs a browser.
 */

export const HISTORY_ID = "70cad697-02d0-4f64-b7ae-a91bbbbf3404";
export const WIDTH = 64;
export const HEIGHT = 48;

export function uuidBytes(uuid: string): Uint8Array {
  return Uint8Array.from(uuid.replace(/-/g, "").match(/../g)!.map((b) => parseInt(b, 16)));
}

export function u64(value: number): Uint8Array {
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigUint64(0, BigInt(value), true);
  return out;
}

export function concat(...parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const out = new Uint8Array(total);
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

export const welcome = (sessionId: number) => Uint8Array.from([MSG_TYPE.WELCOME, sessionId]);

export const replayStart = (afterSeq: number, lastSeq: number, historyId = HISTORY_ID) =>
  concat(Uint8Array.from([MSG_TYPE.REPLAY_START]), uuidBytes(historyId), u64(afterSeq), u64(lastSeq));

export const caughtUp = (lastSeq: number, historyId = HISTORY_ID) =>
  concat(Uint8Array.from([MSG_TYPE.CAUGHT_UP]), uuidBytes(historyId), u64(lastSeq));

export const sequenced = (seq: number, payload: Uint8Array, historyId = HISTORY_ID) =>
  concat(Uint8Array.from([MSG_TYPE.SEQUENCED]), uuidBytes(historyId), u64(seq), payload);

/** Several sequenced messages in one compressed frame, as a join receives them. */
export const replayBatch = (
  entries: { seq: number; payload: Uint8Array }[], historyId = HISTORY_ID,
) => {
  const body = concat(...entries.flatMap(({ seq, payload }) => {
    const length = new Uint8Array(4);
    new DataView(length.buffer).setUint32(0, payload.length, true);
    return [u64(seq), length, payload];
  }));
  const count = new Uint8Array(4);
  new DataView(count.buffer).setUint32(0, entries.length, true);
  return concat(
    Uint8Array.from([MSG_TYPE.REPLAY_BATCH]), uuidBytes(historyId), count, deflateZlib(body),
  );
};

export const resetPoint = (baseSeq: number, count: number) => {
  const out = new Uint8Array(11);
  out[0] = MSG_TYPE.RESET_POINT;
  out.set(u64(baseSeq), 1);
  new DataView(out.buffer).setUint16(9, count, true);
  return out;
};

export const snapshot = (
  uploader: number, subject: number, layer: "background" | "foreground", png: Uint8Array,
) => {
  const head = new Uint8Array(8);
  head[0] = MSG_TYPE.SNAPSHOT;
  head[1] = uploader;
  head[2] = subject;
  // LAYER.FOREGROUND is 0; see binaryProtocol.
  head[3] = layer === "foreground" ? 0 : 1;
  new DataView(head.buffer).setUint32(4, png.length, true);
  return concat(head, png);
};

/** A short opaque stroke, encoded exactly as a client would send one. */
export function stroke(userId: number, at: { x: number; y: number }): Uint8Array {
  const operation: PainterOperation = {
    kind: "stroke",
    layer: "foreground",
    brushSize: 4,
    brush: "solid",
    color: { r: 0, g: 0, b: 0, a: 255 },
    points: [
      { x: at.x, y: at.y },
      { x: at.x + 3, y: at.y },
    ],
    mask: { type: 0, r: 0, g: 0, b: 0 },
  };
  return new Uint8Array(encodePainterOperation(userId, operation));
}
