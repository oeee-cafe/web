import {
  inflateZlib,
  type PainterOperation,
  type PainterBrush as WireBrushType,
  type PainterMask as Mask,
  type PainterRegionTool as RegionTool,
} from "neo-cucumber";

const NO_MASK: Mask = { type: 0, r: 0, g: 0, b: 0 };
/**
 * Binary WebSocket protocol for efficient collaborative drawing
 *
 * Message Types:
 * - 0x00-0x0F: Server messages (parsed by server)
 * - 0x10: REPLAY_BATCH, server to client only; the server range is full
 * - 0x11+: Client messages (broadcast only)
 *
 * Canvas messages carry a 1-byte session-scoped user id (assigned by the
 * server via WELCOME, Drawpile's "context id") instead of a 16-byte UUID.
 * Identity/presence messages (join, chat, layers...) still use UUIDs.
 */

/**
 * The mask a stroke was drawn through: NEO's MASKTYPE (0-4) and the colour it
 * tests against.
 *
 * Every message that carries drawing state carries one, always -- an absent
 * mask is `type: 0`, not an absent field. Making it optional would mean the
 * receiver had to infer presence from the buffer's length, and a stroke that
 * silently loses its mask draws different pixels on every other screen.
 *
 * It sits after the payload rather than in the header because these messages
 * already end with a variable-length section, so a fixed-size trailer leaves
 * every other offset alone.
 */
export type WireMask = Mask;

/** MASKTYPE_NONE, which is what almost every stroke carries. */
export const NO_WIRE_MASK = NO_MASK;

/** Bytes the mask occupies on the wire. */
export const MASK_BYTES = 4;

/** Returns `base` with the mask written into its last four bytes. */
function withMask(base: Uint8Array, mask: WireMask | undefined): ArrayBuffer {
  const m = mask ?? NO_MASK;
  const end = base.length - MASK_BYTES;
  base[end] = m.type;
  base[end + 1] = m.r;
  base[end + 2] = m.g;
  base[end + 3] = m.b;
  return base.buffer as ArrayBuffer;
}

/** Reads the mask occupying the four bytes at `end`. */
function readMask(buffer: Uint8Array, end: number): WireMask {
  return {
    type: buffer[end] as Mask["type"],
    r: buffer[end + 1],
    g: buffer[end + 2],
    b: buffer[end + 3],
  };
}

// Message type constants
export const MSG_TYPE = {
  // Server messages (< 0x10) - server parses and handles
  JOIN: 0x01,
  SNAPSHOT: 0x02,
  CHAT: 0x03,
  // Answers a reset query: this client is caught up and can upload the
  // checkpoint (client -> server)
  RESET_OFFER: 0x04,
  // Announces the accepted replay start and target before history arrives.
  REPLAY_START: 0x05,
  LAYERS: 0x06,
  END_SESSION: 0x07,
  SESSION_EXPIRED: 0x08,
  LEAVE: 0x09,
  // Wraps a history message with its history identity and canonical position.
  SEQUENCED: 0x0a,
  // Asks this client to upload a session reset (server -> client)
  RESET_REQUEST: 0x0b,
  // Announces a session reset upload: base seq + snapshot count (client -> server)
  RESET_BEGIN: 0x0c,
  // Notifies clients that history at or below a base seq was squashed (server -> client)
  RESET_POINT: 0x0d,
  // Tells a connecting client its 1-byte session user id (server -> client)
  WELCOME: 0x0e,
  // Declares the history identity and exact position reached by replay.
  CAUGHT_UP: 0x0f,
  // Several sequenced history messages in one compressed frame, sent to a
  // client whose URL asked for its replay that way. The server refuses the
  // byte from a client, so one can only come from the server.
  REPLAY_BATCH: 0x10,

  // Client messages (> 0x10) - server just broadcasts
  FILL: 0x12,
  POINTER_UP: 0x13,
  // Ephemeral collaborator pointer position; never enters canvas history.
  MOVE_POINTER: 0x1c,
  // Marks the start of an undoable operation (stroke or fill)
  UNDO_POINT: 0x14,
  // Undo (or redo) the sender's most recent operation
  UNDO: 0x15,
  // A batch of contiguous polyline points sharing one set of brush properties
  STROKE: 0x16,
  // A region tool applied to a dragged-out rectangle
  REGION: 0x17,
  // A straight line between two points
  LINE: 0x18,
  // A cubic bezier through four control points
  BEZIER: 0x19,
  // A layer cleared in one go
  ERASE_ALL: 0x1a,
  // Text committed at a point
  TEXT: 0x1b,
  // A rectangle of pixels, replacing what is under it. A flood fill travels
  // this way: as the region it covered rather than as the seed that made it.
  PUT_IMAGE: 0x1d,
} as const;

/**
 * Wire codes for region tools. Append only, for the same reason brush codes
 * are: a client that predates a code cannot learn it, and renumbering would
 * silently reinterpret history as a different tool.
 */
export const REGION_TOOL = {
  ERASE_RECT: 0,
  BLUR_RECT: 1,
  MERGE: 2,
  FLIP_H: 3,
  FLIP_V: 4,
  TURN: 5,
  RECT: 6,
  RECT_FILL: 7,
  ELLIPSE: 8,
  ELLIPSE_FILL: 9,
  COPY: 10,
  PASTE: 11,
} as const;

const REGION_TOOL_TO_CODE: Record<RegionTool, number> = {
  eraseRect: REGION_TOOL.ERASE_RECT,
  blurRect: REGION_TOOL.BLUR_RECT,
  merge: REGION_TOOL.MERGE,
  flipH: REGION_TOOL.FLIP_H,
  flipV: REGION_TOOL.FLIP_V,
  turn: REGION_TOOL.TURN,
  rect: REGION_TOOL.RECT,
  rectFill: REGION_TOOL.RECT_FILL,
  ellipse: REGION_TOOL.ELLIPSE,
  ellipseFill: REGION_TOOL.ELLIPSE_FILL,
  copy: REGION_TOOL.COPY,
  paste: REGION_TOOL.PASTE,
};

const CODE_TO_REGION_TOOL: Record<number, RegionTool> = Object.fromEntries(
  Object.entries(REGION_TOOL_TO_CODE).map(([tool, code]) => [code, tool])
) as Record<number, RegionTool>;

// Layer constants
export const LAYER = {
  FOREGROUND: 0,
  BACKGROUND: 1,
} as const;

// Brush type constants
/**
 * Wire codes for brush types. Append only: a client that predates a code has
 * no way to learn it, so renumbering these would silently redraw history.
 */
export const BRUSH_TYPE = {
  SOLID: 0,
  HALFTONE: 1,
  ERASER: 2,
  BRUSH: 3,
  DODGE: 4,
  BURN: 5,
  BLUR: 6,
} as const;

const BRUSH_TYPE_TO_CODE: Record<WireBrushType, number> = {
  solid: BRUSH_TYPE.SOLID,
  halftone: BRUSH_TYPE.HALFTONE,
  eraser: BRUSH_TYPE.ERASER,
  brush: BRUSH_TYPE.BRUSH,
  dodge: BRUSH_TYPE.DODGE,
  burn: BRUSH_TYPE.BURN,
  blur: BRUSH_TYPE.BLUR,
};

const CODE_TO_BRUSH_TYPE: Record<number, WireBrushType> = {
  [BRUSH_TYPE.SOLID]: "solid",
  [BRUSH_TYPE.HALFTONE]: "halftone",
  [BRUSH_TYPE.ERASER]: "eraser",
  [BRUSH_TYPE.BRUSH]: "brush",
  [BRUSH_TYPE.DODGE]: "dodge",
  [BRUSH_TYPE.BURN]: "burn",
  [BRUSH_TYPE.BLUR]: "blur",
};

// Pointer type constants
export const POINTER_TYPE = {
  MOUSE: 0,
  PEN: 1,
  TOUCH: 2,
} as const;

/**
 * Convert UUID string to 16-byte array
 */
export function uuidToBytes(uuid: string): Uint8Array {
  const hex = uuid.replace(/-/g, "");
  const bytes = new Uint8Array(16);
  for (let i = 0; i < 16; i++) {
    bytes[i] = parseInt(hex.substring(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

/**
 * Convert 16-byte array to UUID string
 */
export function bytesToUuid(bytes: Uint8Array): string {
  const hex = Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");

  return [
    hex.slice(0, 8),
    hex.slice(8, 12),
    hex.slice(12, 16),
    hex.slice(16, 20),
    hex.slice(20, 32),
  ].join("-");
}

/**
 * Write little-endian uint16 to buffer
 */
function writeUint16LE(
  buffer: Uint8Array,
  offset: number,
  value: number
): void {
  buffer[offset] = value & 0xff;
  buffer[offset + 1] = (value >> 8) & 0xff;
}

/**
 * Write little-endian int16 to buffer
 */
function writeInt16LE(buffer: Uint8Array, offset: number, value: number): void {
  const uint16Value = value < 0 ? value + 65536 : value;
  buffer[offset] = uint16Value & 0xff;
  buffer[offset + 1] = (uint16Value >> 8) & 0xff;
}

/**
 * Write little-endian uint32 to buffer
 */
function writeUint32LE(
  buffer: Uint8Array,
  offset: number,
  value: number
): void {
  buffer[offset] = value & 0xff;
  buffer[offset + 1] = (value >> 8) & 0xff;
  buffer[offset + 2] = (value >> 16) & 0xff;
  buffer[offset + 3] = (value >> 24) & 0xff;
}

/**
 * Write little-endian uint64 to buffer
 */
function writeUint64LE(
  buffer: Uint8Array,
  offset: number,
  value: number
): void {
  writeUint32LE(buffer, offset, value & 0xffffffff);
  writeUint32LE(buffer, offset + 4, Math.floor(value / 0x100000000));
}

/**
 * Read little-endian uint16 from buffer
 */
function readUint16LE(buffer: Uint8Array, offset: number): number {
  return buffer[offset] | (buffer[offset + 1] << 8);
}

/**
 * Read little-endian int16 from buffer
 */
function readInt16LE(buffer: Uint8Array, offset: number): number {
  const uint16Value = buffer[offset] | (buffer[offset + 1] << 8);
  return uint16Value >= 32768 ? uint16Value - 65536 : uint16Value;
}

/**
 * Read little-endian uint32 from buffer
 *
 * `>>> 0` at the end, because `|` works on signed 32-bit integers: a top byte
 * of 0x80 or more came back negative, and every 64-bit value built on top of
 * it -- the millisecond timestamps on JOIN, LEAVE, CHAT and the participant
 * list -- read 2^32 ms (49.7 days) early for half of every 49.7-day cycle.
 */
function readUint32LE(buffer: Uint8Array, offset: number): number {
  return (
    (buffer[offset] |
      (buffer[offset + 1] << 8) |
      (buffer[offset + 2] << 16) |
      (buffer[offset + 3] << 24)) >>>
    0
  );
}

/**
 * Read little-endian uint64 from buffer
 */
function readUint64LE(buffer: Uint8Array, offset: number): number {
  const low = readUint32LE(buffer, offset);
  const high = readUint32LE(buffer, offset + 4);
  return low + high * 0x100000000;
}

/**
 * Encode JOIN message (0x01)
 * Format: [0x01][UUID:16][timestamp:8]
 */
export function encodeJoin(userId: string, timestamp: number): ArrayBuffer {
  const buffer = new Uint8Array(25);
  buffer[0] = MSG_TYPE.JOIN;
  buffer.set(uuidToBytes(userId), 1);
  writeUint64LE(buffer, 17, timestamp);
  return buffer.buffer;
}

/**
 * Encode SNAPSHOT message (0x02)
 * Format: [0x02][id:1][layer:1][pngLength:4][pngData:variable]
 */
export async function encodeSnapshot(
  userId: number,
  targetOwner: number,
  layer: "foreground" | "background",
  pngBlob: Blob
): Promise<ArrayBuffer> {
  const pngBytes = new Uint8Array(await pngBlob.arrayBuffer());
  const buffer = new Uint8Array(8 + pngBytes.length);

  buffer[0] = MSG_TYPE.SNAPSHOT;
  buffer[1] = userId;
  buffer[2] = targetOwner;
  buffer[3] = layer === "foreground" ? LAYER.FOREGROUND : LAYER.BACKGROUND;
  writeUint32LE(buffer, 4, pngBytes.length);
  buffer.set(pngBytes, 8);

  return buffer.buffer;
}

/**
 * Encode PUT_IMAGE message (0x1D)
 *
 * A flood fill as the ground it covered: a box, the one colour every covered
 * pixel became, and a bit per pixel saying which ones those were --
 * DEFLATE-compressed, which `CompressionStream` gives us for nothing.
 *
 * Not a picture of the box. That would carry whatever else lies inside it,
 * and stamping it back down on a replay resurrects work since undone.
 *
 * Format: [0x1D][id:1][owner:1][layer:1][x:2][y:2][w:2][h:2][rgba:4][len:4][coverage]
 */
export function encodePutImage(
  userId: number,
  targetOwner: number,
  layer: "foreground" | "background",
  x: number,
  y: number,
  width: number,
  height: number,
  color: { r: number; g: number; b: number; a: number },
  compressed: Uint8Array
): ArrayBuffer {
  const buffer = new Uint8Array(20 + compressed.length);

  buffer[0] = MSG_TYPE.PUT_IMAGE;
  buffer[1] = userId;
  buffer[2] = targetOwner;
  buffer[3] = layer === "foreground" ? LAYER.FOREGROUND : LAYER.BACKGROUND;
  writeUint16LE(buffer, 4, x);
  writeUint16LE(buffer, 6, y);
  writeUint16LE(buffer, 8, width);
  writeUint16LE(buffer, 10, height);
  buffer[12] = color.r;
  buffer[13] = color.g;
  buffer[14] = color.b;
  buffer[15] = color.a;
  writeUint32LE(buffer, 16, compressed.length);
  buffer.set(compressed, 20);

  return buffer.buffer;
}

/**
 * Encode STROKE message (0x16)
 * A batch of contiguous polyline points sharing one set of brush properties.
 * Each point continues the sender's stroke from their previous point (or
 * starts a dot after an UNDO_POINT reset).
 * Format: [0x16][id:1][layer:1][brushSize:1][brushType:1][r:1][g:1][b:1][a:1][count:2][(x:2,y:2)*count]
 */
export function encodeStroke(
  userId: number,
  targetOwner: number,
  layer: "foreground" | "background",
  brushSize: number,
  brushType: WireBrushType,
  r: number,
  g: number,
  b: number,
  a: number,
  points: { x: number; y: number }[],
  mask?: WireMask
): ArrayBuffer {
  const buffer = new Uint8Array(12 + points.length * 4 + MASK_BYTES);

  buffer[0] = MSG_TYPE.STROKE;
  buffer[1] = userId;
  buffer[2] = targetOwner;
  buffer[3] = layer === "foreground" ? LAYER.FOREGROUND : LAYER.BACKGROUND;
  buffer[4] = brushSize;
  buffer[5] = BRUSH_TYPE_TO_CODE[brushType] ?? BRUSH_TYPE.SOLID;
  buffer[6] = r;
  buffer[7] = g;
  buffer[8] = b;
  buffer[9] = a;
  writeUint16LE(buffer, 10, points.length);
  for (let i = 0; i < points.length; i++) {
    writeInt16LE(buffer, 12 + i * 4, Math.round(points[i].x));
    writeInt16LE(buffer, 14 + i * 4, Math.round(points[i].y));
  }

  return withMask(buffer, mask);
}

/**
 * Encode FILL message (0x12)
 * Format: [0x12][id:1][layer:1][x:2][y:2][r:1][g:1][b:1][a:1]
 */
export function encodeFill(
  userId: number,
  targetOwner: number,
  layer: "foreground" | "background",
  x: number,
  y: number,
  r: number,
  g: number,
  b: number,
  a: number,
  mask?: WireMask
): ArrayBuffer {
  const buffer = new Uint8Array(12 + MASK_BYTES);

  buffer[0] = MSG_TYPE.FILL;
  buffer[1] = userId;
  buffer[2] = targetOwner;
  buffer[3] = layer === "foreground" ? LAYER.FOREGROUND : LAYER.BACKGROUND;
  writeInt16LE(buffer, 4, Math.round(x));
  writeInt16LE(buffer, 6, Math.round(y));
  buffer[8] = r;
  buffer[9] = g;
  buffer[10] = b;
  buffer[11] = a;

  return withMask(buffer, mask);
}

/**
 * Encode CHAT message (0x03)
 * Format: [0x03][UUID:16][timestamp:8][msgLength:2][msgData:variable(UTF-8)]
 */
export function encodeChat(
  userId: string,
  message: string,
  timestamp: number
): ArrayBuffer {
  const encoder = new TextEncoder();
  const msgBytes = encoder.encode(message);
  const buffer = new Uint8Array(27 + msgBytes.length);

  buffer[0] = MSG_TYPE.CHAT;
  buffer.set(uuidToBytes(userId), 1);
  writeUint64LE(buffer, 17, timestamp);
  writeUint16LE(buffer, 25, msgBytes.length);
  buffer.set(msgBytes, 27);

  return buffer.buffer;
}


/**
 * Encode RESET_BEGIN message (0x0C)
 * Announces a session reset upload: the following `count` snapshot messages
 * represent the full canvas state at canonical history position `lastSeq`.
 * Format: [0x0C][lastSeq:8][count:2]
 */
export function encodeResetBegin(lastSeq: number, count: number): ArrayBuffer {
  const buffer = new Uint8Array(11);
  buffer[0] = MSG_TYPE.RESET_BEGIN;
  writeUint64LE(buffer, 1, lastSeq);
  writeUint16LE(buffer, 9, count);
  return buffer.buffer;
}

/**
 * Encode RESET_OFFER message (0x04)
 *
 * "I am caught up and can upload the checkpoint." The server knows which
 * connection sent it, so the message is the answer and nothing else.
 * Format: [0x04]
 */
export function encodeResetOffer(): ArrayBuffer {
  return new Uint8Array([MSG_TYPE.RESET_OFFER]).buffer;
}

/**
 * Encode UNDO_POINT message (0x14)
 * Format: [0x14][id:1]
 */
export function encodeUndoPoint(userId: number): ArrayBuffer {
  const buffer = new Uint8Array(2);
  buffer[0] = MSG_TYPE.UNDO_POINT;
  buffer[1] = userId;
  return buffer.buffer;
}

/**
 * Encode UNDO message (0x15)
 * Format: [0x15][id:1][redo:1]
 */
export function encodeUndo(userId: number, redo: boolean): ArrayBuffer {
  const buffer = new Uint8Array(3);
  buffer[0] = MSG_TYPE.UNDO;
  buffer[1] = userId;
  buffer[2] = redo ? 1 : 0;
  return buffer.buffer;
}

/**
 * Unwrap a SEQUENCED envelope (0x0A):
 * [0x0A][history UUID:16][seq:8][payload]
 * Returns null if the buffer is not a sequenced envelope.
 */
export function unwrapSequenced(
  data: ArrayBuffer
): { historyId: string; seq: number; payload: ArrayBuffer } | null {
  const buffer = new Uint8Array(data);
  if (buffer.length < 26 || buffer[0] !== MSG_TYPE.SEQUENCED) {
    return null;
  }
  return {
    historyId: bytesToUuid(buffer.slice(1, 17)),
    seq: readUint64LE(buffer, 17),
    payload: data.slice(25),
  };
}

/**
 * Unwraps a REPLAY_BATCH into the sequenced messages it carries.
 *
 * `[0x10][historyId:16][count:4][zlib([seq:8][len:4][message]...)]`. Each
 * entry is what a SEQUENCED frame of the same message carries after its
 * envelope. A batch that does not inflate, or whose entries do not add up to
 * its count, is refused whole rather than applied in part: a replay missing
 * a message would fail the position check and be asked for again, but one
 * with a message cut short would put a wrong operation on the canvas.
 */
export function unwrapReplayBatch(
  data: ArrayBuffer,
): { historyId: string; entries: { seq: number; payload: ArrayBuffer }[] } | null {
  const buffer = new Uint8Array(data);
  if (buffer.length < 21 || buffer[0] !== MSG_TYPE.REPLAY_BATCH) return null;
  const historyId = bytesToUuid(buffer.slice(1, 17));
  const count = readUint32LE(buffer, 17);
  let body: Uint8Array;
  try {
    body = inflateZlib(buffer.subarray(21));
  } catch {
    return null;
  }
  const entries: { seq: number; payload: ArrayBuffer }[] = [];
  let at = 0;
  while (at < body.length) {
    if (at + 12 > body.length) return null;
    const seq = readUint64LE(body, at);
    const length = readUint32LE(body, at + 8);
    at += 12;
    if (at + length > body.length) return null;
    entries.push({ seq, payload: body.slice(at, at + length).buffer as ArrayBuffer });
    at += length;
  }
  if (entries.length !== count) return null;
  return { historyId, entries };
}

/**
 * Encode END_SESSION message (0x07)
 * Format: [0x07][UUID:16][postUrlLength:2][postUrl:variable(UTF-8)]
 */
export function encodeEndSession(userId: string, postUrl: string): ArrayBuffer {
  const encoder = new TextEncoder();
  const urlBytes = encoder.encode(postUrl);
  const buffer = new Uint8Array(19 + urlBytes.length);

  buffer[0] = MSG_TYPE.END_SESSION;
  buffer.set(uuidToBytes(userId), 1);
  writeUint16LE(buffer, 17, urlBytes.length);
  buffer.set(urlBytes, 19);

  return buffer.buffer;
}

/**
 * Encode POINTER_UP message (0x13)
 * Format: [0x13][id:1]
 */
export function encodePointerUp(userId: number): ArrayBuffer {
  const buffer = new Uint8Array(2);
  buffer[0] = MSG_TYPE.POINTER_UP;
  buffer[1] = userId;
  return buffer.buffer;
}

/** Drawpile-style quarter-pixel fixed-point pointer position. */
export function encodeMovePointer(userId: number, x: number, y: number): ArrayBuffer {
  const buffer = new ArrayBuffer(10);
  const bytes = new Uint8Array(buffer);
  const view = new DataView(buffer);
  bytes[0] = MSG_TYPE.MOVE_POINTER;
  bytes[1] = userId;
  view.setInt32(2, Math.round(x * 4), true);
  view.setInt32(6, Math.round(y * 4), true);
  return buffer;
}

// Decoded message types
/**
 * Encode REGION message (0x17)
 * Format: [0x17][id:1][layer:1][tool:1][x:2][y:2][w:2][h:2][rgba:4][size:1]
 *
 * The rectangle is sent already clipped and derived, so every client applies
 * the identical rect rather than re-deriving it from a drag it never saw.
 */
export function encodeRegion(
  userId: number,
  targetOwner: number,
  layer: "foreground" | "background",
  tool: RegionTool,
  rect: { x: number; y: number; width: number; height: number },
  color: { r: number; g: number; b: number; a: number },
  brushSize: number,
  mask?: WireMask
): ArrayBuffer {
  const buffer = new Uint8Array(18 + MASK_BYTES);
  buffer[0] = MSG_TYPE.REGION;
  buffer[1] = userId;
  buffer[2] = targetOwner;
  buffer[3] = layer === "foreground" ? LAYER.FOREGROUND : LAYER.BACKGROUND;
  buffer[4] = REGION_TOOL_TO_CODE[tool];
  writeInt16LE(buffer, 5, Math.round(rect.x));
  writeInt16LE(buffer, 7, Math.round(rect.y));
  writeInt16LE(buffer, 9, Math.round(rect.width));
  writeInt16LE(buffer, 11, Math.round(rect.height));
  buffer[13] = color.r;
  buffer[14] = color.g;
  buffer[15] = color.b;
  buffer[16] = color.a;
  buffer[17] = brushSize;
  return withMask(buffer, mask);
}

/**
 * Encode LINE message (0x18)
 * Format: [0x18][id:1][layer:1][brush:1][size:1][rgba:4][x0:2][y0:2][x1:2][y1:2]
 */
export function encodeLine(
  userId: number,
  targetOwner: number,
  layer: "foreground" | "background",
  brushSize: number,
  brushType: WireBrushType,
  color: { r: number; g: number; b: number; a: number },
  from: { x: number; y: number },
  to: { x: number; y: number },
  mask?: WireMask
): ArrayBuffer {
  const buffer = new Uint8Array(18 + MASK_BYTES);
  buffer[0] = MSG_TYPE.LINE;
  buffer[1] = userId;
  buffer[2] = targetOwner;
  buffer[3] = layer === "foreground" ? LAYER.FOREGROUND : LAYER.BACKGROUND;
  buffer[4] = BRUSH_TYPE_TO_CODE[brushType] ?? BRUSH_TYPE.SOLID;
  buffer[5] = brushSize;
  buffer[6] = color.r;
  buffer[7] = color.g;
  buffer[8] = color.b;
  buffer[9] = color.a;
  writeInt16LE(buffer, 10, Math.round(from.x));
  writeInt16LE(buffer, 12, Math.round(from.y));
  writeInt16LE(buffer, 14, Math.round(to.x));
  writeInt16LE(buffer, 16, Math.round(to.y));
  return withMask(buffer, mask);
}

/**
 * Encode BEZIER message (0x19)
 * Format: [0x19][id:1][layer:1][brush:1][size:1][rgba:4][8 x int16]
 * Points are in NEO's order: start, both handles, end.
 */
export function encodeBezier(
  userId: number,
  targetOwner: number,
  layer: "foreground" | "background",
  brushSize: number,
  brushType: WireBrushType,
  color: { r: number; g: number; b: number; a: number },
  points: number[],
  mask?: WireMask
): ArrayBuffer {
  const buffer = new Uint8Array(26 + MASK_BYTES);
  buffer[0] = MSG_TYPE.BEZIER;
  buffer[1] = userId;
  buffer[2] = targetOwner;
  buffer[3] = layer === "foreground" ? LAYER.FOREGROUND : LAYER.BACKGROUND;
  buffer[4] = BRUSH_TYPE_TO_CODE[brushType] ?? BRUSH_TYPE.SOLID;
  buffer[5] = brushSize;
  buffer[6] = color.r;
  buffer[7] = color.g;
  buffer[8] = color.b;
  buffer[9] = color.a;
  for (let i = 0; i < 8; i++) {
    writeInt16LE(buffer, 10 + i * 2, Math.round(points[i] ?? 0));
  }
  return withMask(buffer, mask);
}

/**
 * Encode ERASE_ALL message (0x1a)
 * Format: [0x1a][id:1][layer:1]
 */
export function encodeEraseAll(
  userId: number,
  targetOwner: number,
  layer: "foreground" | "background"
): ArrayBuffer {
  const buffer = new Uint8Array(4);
  buffer[0] = MSG_TYPE.ERASE_ALL;
  buffer[1] = userId;
  buffer[2] = targetOwner;
  buffer[3] = layer === "foreground" ? LAYER.FOREGROUND : LAYER.BACKGROUND;
  return buffer.buffer;
}

/**
 * Encode TEXT message (0x1b)
 * Format: [0x1b][id:1][layer:1][size:1][rgba:4][x:2][y:2][len:2][utf8:variable]
 *
 * The pen size travels rather than a font size, because the receiver derives
 * one from the other exactly as NEO does; sending both invites them to drift.
 */
export function encodeText(
  userId: number,
  targetOwner: number,
  layer: "foreground" | "background",
  x: number,
  y: number,
  text: string,
  color: { r: number; g: number; b: number; a: number },
  brushSize: number,
  mask?: WireMask
): ArrayBuffer {
  const encoded = new TextEncoder().encode(text);
  const buffer = new Uint8Array(15 + encoded.length + MASK_BYTES);
  buffer[0] = MSG_TYPE.TEXT;
  buffer[1] = userId;
  buffer[2] = targetOwner;
  buffer[3] = layer === "foreground" ? LAYER.FOREGROUND : LAYER.BACKGROUND;
  buffer[4] = brushSize;
  buffer[5] = color.r;
  buffer[6] = color.g;
  buffer[7] = color.b;
  buffer[8] = color.a;
  writeInt16LE(buffer, 9, Math.round(x));
  writeInt16LE(buffer, 11, Math.round(y));
  writeUint16LE(buffer, 13, encoded.length);
  buffer.set(encoded, 15);
  return withMask(buffer, mask);
}

export interface JoinMessage {
  type: "join";
  userId: string;
  username: string;
  timestamp: number;
}

export interface LayersMessage {
  type: "layers";
  participants: Array<{
    userId: string;
    sessionId: number;
    username: string;
    joinTimestamp: number;
  }>;
}

export interface WelcomeMessage {
  type: "welcome";
  sessionId: number;
}

export interface PutImageMessage {
  type: "putImage";
  userId: number;
  /** Whose layer pair the pixels land in; the author is `userId`. */
  targetOwner: number;
  layer: "foreground" | "background";
  x: number;
  y: number;
  width: number;
  height: number;
  /** The one colour every covered pixel becomes. */
  color: { r: number; g: number; b: number; a: number };
  /** A bit per pixel of the box, DEFLATE-compressed. */
  coverage: Uint8Array;
}

export interface SnapshotMessage {
  type: "snapshot";
  userId: number;
  /** Whose layer pair the pixels land in; the author is `userId`. */
  targetOwner: number;
  layer: "foreground" | "background";
  pngData: Uint8Array;
}

export interface StrokeMessage {
  type: "stroke";
  userId: number;
  /** Whose layer pair the pixels land in; the author is `userId`. */
  targetOwner: number;
  layer: "foreground" | "background";
  brushSize: number;
  brushType: WireBrushType;
  color: { r: number; g: number; b: number; a: number };
  points: { x: number; y: number }[];
  /** NEO's MASKTYPE and its colour; absent on history written before masks. */
  mask: WireMask;
}

export interface FillMessage {
  type: "fill";
  userId: number;
  /** Whose layer pair the pixels land in; the author is `userId`. */
  targetOwner: number;
  layer: "foreground" | "background";
  x: number;
  y: number;
  color: { r: number; g: number; b: number; a: number };
  mask: WireMask;
}

export interface RegionMessage {
  type: "region";
  userId: number;
  /** Whose layer pair the pixels land in; the author is `userId`. */
  targetOwner: number;
  layer: "foreground" | "background";
  tool: RegionTool;
  rect: { x: number; y: number; width: number; height: number };
  color: { r: number; g: number; b: number; a: number };
  brushSize: number;
  mask: WireMask;
}

export interface LineMessage {
  type: "line";
  userId: number;
  /** Whose layer pair the pixels land in; the author is `userId`. */
  targetOwner: number;
  layer: "foreground" | "background";
  brushSize: number;
  brushType: WireBrushType;
  color: { r: number; g: number; b: number; a: number };
  from: { x: number; y: number };
  to: { x: number; y: number };
  mask: WireMask;
}

export interface BezierMessage {
  type: "bezier";
  userId: number;
  /** Whose layer pair the pixels land in; the author is `userId`. */
  targetOwner: number;
  layer: "foreground" | "background";
  brushSize: number;
  brushType: WireBrushType;
  color: { r: number; g: number; b: number; a: number };
  points: number[];
  mask: WireMask;
}

export interface EraseAllMessage {
  type: "eraseAll";
  userId: number;
  /** Whose layer pair the pixels land in; the author is `userId`. */
  targetOwner: number;
  layer: "foreground" | "background";
}

export interface TextMessage {
  type: "text";
  userId: number;
  /** Whose layer pair the pixels land in; the author is `userId`. */
  targetOwner: number;
  layer: "foreground" | "background";
  x: number;
  y: number;
  text: string;
  color: { r: number; g: number; b: number; a: number };
  brushSize: number;
  mask: WireMask;
}

export interface ChatMessage {
  type: "chat";
  userId: string;
  username: string;
  timestamp: number;
  message: string;
}

/**
 * What the server wants of us about a checkpoint.
 *
 * `query` goes to the whole room and asks who *can*; `upload` goes to the one
 * client that answered first and asks it to actually do the work. Making a
 * checkpoint means exporting every layer of every participant as a PNG, and a
 * client that is mid catch-up cannot make a correct one at all -- so the
 * server asks before it picks, rather than picking and hoping.
 */
export type ResetPhase = "query" | "upload";

export interface ResetRequestMessage {
  type: "resetRequest";
  timestamp: number;
  phase: ResetPhase;
}

export interface ResetPointMessage {
  type: "resetPoint";
  baseSeq: number;
  /**
   * How many snapshots the checkpoint at `baseSeq` is made of.
   *
   * Every snapshot of a reset is stored at the same sequence, so without this
   * a client cannot tell a half-arrived checkpoint from a whole one -- and
   * with one layer pair per participant there is no fixed number to assume.
   */
  snapshotCount: number;
}

export interface CaughtUpMessage {
  type: "caughtUp";
  historyId: string;
  lastSeq: number;
}

export interface ReplayStartMessage {
  type: "replayStart";
  historyId: string;
  afterSeq: number;
  lastSeq: number;
}

export interface UndoPointMessage {
  type: "undoPoint";
  userId: number;
}

export interface UndoMessage {
  type: "undo";
  userId: number;
  redo: boolean;
}

export interface PointerUpMessage {
  type: "pointerup";
  userId: number;
}

export interface MovePointerMessage {
  type: "movePointer";
  userId: number;
  x: number;
  y: number;
}

export interface EndSessionMessage {
  type: "endSession";
  userId: string;
  postUrl: string;
}

export interface SessionExpiredMessage {
  type: "sessionExpired";
  sessionId: string;
}

export interface LeaveMessage {
  type: "leave";
  userId: string;
  username: string;
  timestamp: number;
}

export type DecodedMessage =
  | RegionMessage
  | LineMessage
  | BezierMessage
  | EraseAllMessage
  | TextMessage
  | JoinMessage
  | LayersMessage
  | WelcomeMessage
  | SnapshotMessage
  | ChatMessage
  | ResetRequestMessage
  | ResetPointMessage
  | CaughtUpMessage
  | ReplayStartMessage
  | UndoPointMessage
  | UndoMessage
  | StrokeMessage
  | FillMessage
  | PutImageMessage
  | PointerUpMessage
  | MovePointerMessage
  | EndSessionMessage
  | SessionExpiredMessage
  | LeaveMessage;

/**
 * Decode SNAPSHOT message specifically
 * This is a convenience function that wraps decodeMessage for snapshot handling
 */
export function decodeSnapshot(data: ArrayBuffer): SnapshotMessage | null {
  const message = decodeMessage(data);
  if (message && message.type === "snapshot") {
    return message as SnapshotMessage;
  }
  return null;
}

/**
 * Decode binary message based on message type
 */
export function decodeMessage(data: ArrayBuffer): DecodedMessage | null {
  const buffer = new Uint8Array(data);
  if (buffer.length === 0) return null;

  const msgType = buffer[0];

  switch (msgType) {
    case MSG_TYPE.REGION: {
      if (buffer.length < 18 + MASK_BYTES) return null;
      const tool = CODE_TO_REGION_TOOL[buffer[4]];
      // A tool code this client predates: drop it rather than guess, since
      // applying the wrong region op is worse than applying none.
      if (!tool) return null;
      return {
        type: "region",
        userId: buffer[1],
        targetOwner: buffer[2],
        layer: buffer[3] === LAYER.FOREGROUND ? "foreground" : "background",
        tool,
        rect: {
          x: readInt16LE(buffer, 5),
          y: readInt16LE(buffer, 7),
          width: readInt16LE(buffer, 9),
          height: readInt16LE(buffer, 11),
        },
        color: { r: buffer[13], g: buffer[14], b: buffer[15], a: buffer[16] },
        brushSize: buffer[17],
        mask: readMask(buffer, 18),
      };
    }

    case MSG_TYPE.LINE: {
      if (buffer.length < 18 + MASK_BYTES) return null;
      return {
        type: "line",
        userId: buffer[1],
        targetOwner: buffer[2],
        layer: buffer[3] === LAYER.FOREGROUND ? "foreground" : "background",
        brushType: CODE_TO_BRUSH_TYPE[buffer[4]] ?? "solid",
        brushSize: buffer[5],
        color: { r: buffer[6], g: buffer[7], b: buffer[8], a: buffer[9] },
        from: { x: readInt16LE(buffer, 10), y: readInt16LE(buffer, 12) },
        to: { x: readInt16LE(buffer, 14), y: readInt16LE(buffer, 16) },
        mask: readMask(buffer, 18),
      };
    }

    case MSG_TYPE.BEZIER: {
      if (buffer.length < 26 + MASK_BYTES) return null;
      const points: number[] = [];
      for (let i = 0; i < 8; i++) points.push(readInt16LE(buffer, 10 + i * 2));
      return {
        type: "bezier",
        userId: buffer[1],
        targetOwner: buffer[2],
        layer: buffer[3] === LAYER.FOREGROUND ? "foreground" : "background",
        brushType: CODE_TO_BRUSH_TYPE[buffer[4]] ?? "solid",
        brushSize: buffer[5],
        color: { r: buffer[6], g: buffer[7], b: buffer[8], a: buffer[9] },
        points,
        mask: readMask(buffer, 26),
      };
    }

    case MSG_TYPE.ERASE_ALL: {
      if (buffer.length < 4) return null;
      return {
        type: "eraseAll",
        userId: buffer[1],
        targetOwner: buffer[2],
        layer: buffer[3] === LAYER.FOREGROUND ? "foreground" : "background",
      };
    }

    case MSG_TYPE.TEXT: {
      if (buffer.length < 15) return null;
      const textLength = readUint16LE(buffer, 13);
      if (buffer.length < 15 + textLength + MASK_BYTES) return null;
      return {
        type: "text",
        userId: buffer[1],
        targetOwner: buffer[2],
        layer: buffer[3] === LAYER.FOREGROUND ? "foreground" : "background",
        brushSize: buffer[4],
        color: { r: buffer[5], g: buffer[6], b: buffer[7], a: buffer[8] },
        x: readInt16LE(buffer, 9),
        y: readInt16LE(buffer, 11),
        text: new TextDecoder().decode(buffer.slice(15, 15 + textLength)),
        mask: readMask(buffer, 15 + textLength),
      };
    }

    case MSG_TYPE.JOIN: {
      // Format: [0x01][UUID:16][timestamp:8][usernameLength:2][username:variable]
      if (buffer.length < 27) return null; // Minimum: 1 + 16 + 8 + 2 + 0

      const joinUsernameLength = readUint16LE(buffer, 25);
      if (buffer.length < 27 + joinUsernameLength) return null;

      const joinDecoder = new TextDecoder();
      return {
        type: "join",
        userId: bytesToUuid(buffer.slice(1, 17)),
        timestamp: readUint64LE(buffer, 17),
        username: joinDecoder.decode(buffer.slice(27, 27 + joinUsernameLength)),
      };
    }

    case MSG_TYPE.LAYERS: {
      if (buffer.length < 3) return null;
      const participantCount = readUint16LE(buffer, 1);

      const participants: Array<{
        userId: string;
        sessionId: number;
        username: string;
        joinTimestamp: number;
      }> = [];
      let offset = 3;

      for (let i = 0; i < participantCount; i++) {
        // uuid (16) + session id (1) + name length (2) + timestamp (8)
        if (offset + 27 > buffer.length) return null;

        const userId = bytesToUuid(buffer.slice(offset, offset + 16));
        offset += 16;

        const sessionId = buffer[offset];
        offset += 1;

        const nameLength = readUint16LE(buffer, offset);
        offset += 2;

        if (offset + nameLength + 8 > buffer.length) return null;

        const username = new TextDecoder().decode(buffer.slice(offset, offset + nameLength));
        offset += nameLength;

        const joinTimestamp = readUint64LE(buffer, offset);
        offset += 8;

        participants.push({ userId, sessionId, username, joinTimestamp });
      }

      return {
        type: "layers",
        participants: participants,
      };
    }

    case MSG_TYPE.WELCOME:
      if (buffer.length < 2) return null;
      return {
        type: "welcome",
        sessionId: buffer[1],
      };

    case MSG_TYPE.CHAT: {
      // Format: [0x03][UUID:16][timestamp:8][usernameLength:2][username:variable][msgLength:2][msgData:variable]
      if (buffer.length < 29) return null; // Minimum: 1 + 16 + 8 + 2 + 0 + 2 + 0

      const usernameLength = readUint16LE(buffer, 25);
      if (buffer.length < 29 + usernameLength) return null;

      const msgLength = readUint16LE(buffer, 27 + usernameLength);
      if (buffer.length < 29 + usernameLength + msgLength) return null;

      const decoder = new TextDecoder();
      return {
        type: "chat",
        userId: bytesToUuid(buffer.slice(1, 17)),
        timestamp: readUint64LE(buffer, 17),
        username: decoder.decode(buffer.slice(27, 27 + usernameLength)),
        message: decoder.decode(
          buffer.slice(29 + usernameLength, 29 + usernameLength + msgLength)
        ),
      };
    }

    case MSG_TYPE.RESET_REQUEST:
      if (buffer.length < 9) return null;
      return {
        type: "resetRequest",
        timestamp: readUint64LE(buffer, 1),
        // A server from before the query phase existed sends nine bytes and
        // means "upload", which is what it always meant.
        phase: buffer.length > 9 && buffer[9] === 0 ? "query" : "upload",
      };

    case MSG_TYPE.REPLAY_START:
      if (buffer.length < 33) return null;
      return {
        type: "replayStart",
        historyId: bytesToUuid(buffer.slice(1, 17)),
        afterSeq: readUint64LE(buffer, 17),
        lastSeq: readUint64LE(buffer, 25),
      };

    case MSG_TYPE.RESET_POINT:
      if (buffer.length < 11) return null;
      return {
        type: "resetPoint",
        baseSeq: readUint64LE(buffer, 1),
        snapshotCount: readUint16LE(buffer, 9),
      };

    case MSG_TYPE.CAUGHT_UP:
      if (buffer.length < 25) return null;
      return {
        type: "caughtUp",
        historyId: bytesToUuid(buffer.slice(1, 17)),
        lastSeq: readUint64LE(buffer, 17),
      };

    case MSG_TYPE.UNDO_POINT:
      if (buffer.length < 2) return null;
      return {
        type: "undoPoint",
        userId: buffer[1],
      };

    case MSG_TYPE.UNDO:
      if (buffer.length < 3) return null;
      return {
        type: "undo",
        userId: buffer[1],
        redo: buffer[2] !== 0,
      };

    case MSG_TYPE.PUT_IMAGE: {
      if (buffer.length < 20) return null;
      const length = readUint32LE(buffer, 16);
      if (buffer.length < 20 + length) return null;
      return {
        type: "putImage",
        userId: buffer[1],
        targetOwner: buffer[2],
        layer: buffer[3] === LAYER.FOREGROUND ? "foreground" : "background",
        x: readUint16LE(buffer, 4),
        y: readUint16LE(buffer, 6),
        width: readUint16LE(buffer, 8),
        height: readUint16LE(buffer, 10),
        color: { r: buffer[12], g: buffer[13], b: buffer[14], a: buffer[15] },
        coverage: buffer.slice(20, 20 + length),
      };
    }

    case MSG_TYPE.SNAPSHOT: {
      if (buffer.length < 8) return null;
      const pngLength = readUint32LE(buffer, 4);
      if (buffer.length < 8 + pngLength) return null;
      return {
        type: "snapshot",
        userId: buffer[1],
        targetOwner: buffer[2],
        layer: buffer[3] === LAYER.FOREGROUND ? "foreground" : "background",
        pngData: buffer.slice(8, 8 + pngLength),
      };
    }

    case MSG_TYPE.STROKE: {
      if (buffer.length < 12) return null;
      const count = readUint16LE(buffer, 10);
      if (buffer.length < 12 + count * 4 + MASK_BYTES) return null;
      const points: { x: number; y: number }[] = [];
      for (let i = 0; i < count; i++) {
        points.push({
          x: readInt16LE(buffer, 12 + i * 4),
          y: readInt16LE(buffer, 14 + i * 4),
        });
      }
      return {
        type: "stroke",
        userId: buffer[1],
        targetOwner: buffer[2],
        layer: buffer[3] === LAYER.FOREGROUND ? "foreground" : "background",
        brushSize: buffer[4],
        // An unrecognised code falls back to solid, not eraser: a client
        // meeting a tool newer than itself should draw something harmless
        // rather than delete the pixels underneath.
        brushType: CODE_TO_BRUSH_TYPE[buffer[5]] ?? "solid",
        color: { r: buffer[6], g: buffer[7], b: buffer[8], a: buffer[9] },
        points,
        mask: readMask(buffer, 12 + count * 4),
      };
    }

    case MSG_TYPE.FILL:
      if (buffer.length < 12 + MASK_BYTES) return null;
      return {
        type: "fill",
        userId: buffer[1],
        targetOwner: buffer[2],
        layer: buffer[3] === LAYER.FOREGROUND ? "foreground" : "background",
        x: readInt16LE(buffer, 4),
        y: readInt16LE(buffer, 6),
        color: { r: buffer[8], g: buffer[9], b: buffer[10], a: buffer[11] },
        mask: readMask(buffer, 12),
      };

    case MSG_TYPE.POINTER_UP:
      if (buffer.length < 2) return null;
      return {
        type: "pointerup",
        userId: buffer[1],
      };

    case MSG_TYPE.MOVE_POINTER: {
      if (buffer.length < 10) return null;
      const view = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength);
      return {
        type: "movePointer",
        userId: buffer[1],
        x: view.getInt32(2, true) / 4,
        y: view.getInt32(6, true) / 4,
      };
    }

    case MSG_TYPE.END_SESSION: {
      if (buffer.length < 19) return null;
      const urlLength = readUint16LE(buffer, 17);
      if (buffer.length < 19 + urlLength) return null;
      const decoder = new TextDecoder();
      return {
        type: "endSession",
        userId: bytesToUuid(buffer.slice(1, 17)),
        postUrl: decoder.decode(buffer.slice(19, 19 + urlLength)),
      };
    }

    case MSG_TYPE.SESSION_EXPIRED:
      if (buffer.length < 17) return null;
      return {
        type: "sessionExpired",
        sessionId: bytesToUuid(buffer.slice(1, 17)),
      };

    case MSG_TYPE.LEAVE: {
      // Format: [0x09][UUID:16][timestamp:8][usernameLength:2][username:variable]
      if (buffer.length < 27) return null; // Minimum: 1 + 16 + 8 + 2 + 0

      const leaveUsernameLength = readUint16LE(buffer, 25);
      if (buffer.length < 27 + leaveUsernameLength) return null;

      const leaveDecoder = new TextDecoder();
      return {
        type: "leave",
        userId: bytesToUuid(buffer.slice(1, 17)),
        timestamp: readUint64LE(buffer, 17),
        username: leaveDecoder.decode(
          buffer.slice(27, 27 + leaveUsernameLength)
        ),
      };
    }

    default:
      return null;
  }
}

/** Encode the library's transport-neutral operation into oeee's room wire format. */
export function encodePainterOperation(userId: number, operation: PainterOperation): ArrayBuffer {
  // Unsaid means "my own layers": the author and the target are the same
  // person for every mark except one aimed at somebody else's pair.
  const target =
    "targetActorId" in operation && operation.targetActorId !== undefined
      ? Number(operation.targetActorId)
      : userId;
  switch (operation.kind) {
    case "undo-boundary": return encodeUndoPoint(userId);
    case "undo": return encodeUndo(userId, operation.redo);
    case "stroke": return encodeStroke(userId, target, operation.layer, operation.brushSize, operation.brush, operation.color.r, operation.color.g, operation.color.b, operation.color.a, operation.points, operation.mask);
    case "fill": return encodeFill(userId, target, operation.layer, operation.at.x, operation.at.y, operation.color.r, operation.color.g, operation.color.b, operation.color.a, operation.mask);
    case "line": return encodeLine(userId, target, operation.layer, operation.brushSize, operation.brush, operation.color, operation.from, operation.to, operation.mask);
    case "bezier": return encodeBezier(userId, target, operation.layer, operation.brushSize, operation.brush, operation.color, operation.points, operation.mask);
    case "region": return encodeRegion(userId, target, operation.layer, operation.tool, operation.rect, operation.color, operation.brushSize, operation.mask);
    case "text": return encodeText(userId, target, operation.layer, operation.at.x, operation.at.y, operation.text, operation.color, operation.brushSize, operation.mask);
    case "clear-layer": return encodeEraseAll(userId, target, operation.layer);
    // Raster marks are encoded by the caller: the payload is a PNG, and making
    // one is asynchronous where every other encoder here is not.
    // Encoded here like everything else. Handing this one to a different
    // function and trusting callers to notice is how a fill went out to
    // nobody: the operation was made, the encoder threw, and the only screen
    // it ever reached was the one that drew it.
    case "fill-region": return encodePutImage(
      userId, target, operation.layer,
      operation.at.x, operation.at.y, operation.width, operation.height,
      operation.color, operation.coverage,
    );
  }
}

/**
 * The message types that belong in the canonical canvas history.
 *
 * One list, exported, because the client used to keep its own: a kind added to
 * the vocabulary and not to that list was still sequenced by the server and
 * never applied, so every client saw a gap in the sequence, decided it had
 * fallen behind, and dropped the connection. The round-trip test walks every
 * kind in the vocabulary and checks it lands here.
 */
export const CANVAS_HISTORY_MESSAGE_TYPES = [
  "stroke",
  "fill",
  "putImage",
  "region",
  "line",
  "bezier",
  "eraseAll",
  "text",
  "snapshot",
  "undoPoint",
  "undo",
] as const;

export type CanvasHistoryMessageType =
  (typeof CANVAS_HISTORY_MESSAGE_TYPES)[number];

/** Whether this message is one the canonical history has to be given. */
export function isCanvasHistoryMessage(
  message: DecodedMessage,
): message is DecodedMessage & { type: CanvasHistoryMessageType } {
  return (CANVAS_HISTORY_MESSAGE_TYPES as readonly string[]).includes(
    message.type,
  );
}

/** Decode an oeee room drawing message into the library's public vocabulary. */
export function decodePainterOperation(message: DecodedMessage): PainterOperation | null {
  switch (message.type) {
    case "undoPoint": return { kind: "undo-boundary" };
    case "undo": return { kind: "undo", redo: message.redo };
    case "stroke": return { kind: "stroke", targetActorId: String(message.targetOwner), layer: message.layer, brushSize: message.brushSize, brush: message.brushType, color: message.color, points: message.points, mask: message.mask };
    case "fill": return { kind: "fill", targetActorId: String(message.targetOwner), layer: message.layer, at: { x: message.x, y: message.y }, color: message.color, mask: message.mask };
    case "line": return { kind: "line", targetActorId: String(message.targetOwner), layer: message.layer, brushSize: message.brushSize, brush: message.brushType, color: message.color, from: message.from, to: message.to, mask: message.mask };
    case "bezier": return { kind: "bezier", targetActorId: String(message.targetOwner), layer: message.layer, brushSize: message.brushSize, brush: message.brushType, color: message.color, points: message.points as [number, number, number, number, number, number, number, number], mask: message.mask };
    case "region": return { kind: "region", targetActorId: String(message.targetOwner), layer: message.layer, tool: message.tool, rect: message.rect, color: message.color, brushSize: message.brushSize, mask: message.mask };
    case "text": return { kind: "text", targetActorId: String(message.targetOwner), layer: message.layer, at: { x: message.x, y: message.y }, text: message.text, color: message.color, brushSize: message.brushSize, mask: message.mask };
    case "eraseAll": return { kind: "clear-layer", targetActorId: String(message.targetOwner), layer: message.layer };
    case "putImage": return { kind: "fill-region", targetActorId: String(message.targetOwner), layer: message.layer, at: { x: message.x, y: message.y }, width: message.width, height: message.height, color: message.color, coverage: message.coverage, mask: NO_MASK };
    default: return null;
  }
}
