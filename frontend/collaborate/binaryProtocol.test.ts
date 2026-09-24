import { describe, expect, it } from "vitest";
import {
  decodeMessage,
  decodePainterOperation,
  encodeFill,
  encodePainterOperation,
  encodeStroke,
  MASK_BYTES,
  NO_WIRE_MASK,
  encodeBezier,
  encodeEraseAll,
  encodeLine,
  encodeMovePointer,
  encodeRegion,
  encodeResetOffer,
  encodeText,
  MSG_TYPE,
  REGION_TOOL,
  unwrapReplayBatch,
  unwrapSequenced,
  isCanvasHistoryMessage,
} from "./binaryProtocol";
import { HISTORY_ID, replayBatch, stroke, uuidBytes } from "./test/frames";
import { deflateZlib, type PainterOperation, type PainterRegionTool as RegionTool } from "neo-cucumber";

const ID = 3;
const COLOR = { r: 12, g: 200, b: 255, a: 128 };

describe("canonical history positions", () => {
  const historyBytes = new Uint8Array([
    0x12, 0x34, 0x56, 0x78, 0x12, 0x34, 0x56, 0x78,
    0x9a, 0xbc, 0xde, 0xf0, 0x12, 0x34, 0x56, 0x78,
  ]);

  it("unwraps the history identity and sequence together", () => {
    const wire = new Uint8Array(27);
    wire[0] = MSG_TYPE.SEQUENCED;
    wire.set(historyBytes, 1);
    new DataView(wire.buffer).setBigUint64(17, 42n, true);
    wire.set([MSG_TYPE.UNDO, ID], 25);
    expect(unwrapSequenced(wire.buffer)).toEqual({
      historyId: "12345678-1234-5678-9abc-def012345678",
      seq: 42,
      payload: new Uint8Array([MSG_TYPE.UNDO, ID]).buffer,
    });
  });

  it("decodes an explicit caught-up boundary", () => {
    const wire = new Uint8Array(25);
    wire[0] = MSG_TYPE.CAUGHT_UP;
    wire.set(historyBytes, 1);
    new DataView(wire.buffer).setBigUint64(17, 99n, true);
    expect(decodeMessage(wire.buffer)).toEqual({
      type: "caughtUp",
      historyId: "12345678-1234-5678-9abc-def012345678",
      lastSeq: 99,
    });
  });

  it("reads a 64-bit position whose low word has its top bit set", () => {
    // `|` builds a signed 32-bit integer, so a low word of 0x80000000 or more
    // used to come back negative and the whole value 2^32 too small. Every
    // millisecond timestamp on the wire looks like this for half of every
    // 49.7-day cycle.
    const wire = new Uint8Array(25);
    wire[0] = MSG_TYPE.CAUGHT_UP;
    wire.set(historyBytes, 1);
    new DataView(wire.buffer).setBigUint64(17, 0x1_8000_0005n, true);
    expect(decodeMessage(wire.buffer)).toMatchObject({ lastSeq: 0x1_8000_0005 });

    const chat = new Uint8Array(29);
    chat[0] = MSG_TYPE.CHAT;
    new DataView(chat.buffer).setBigUint64(17, 1758789107717n, true);
    expect(decodeMessage(chat.buffer)).toMatchObject({
      type: "chat",
      timestamp: 1758789107717,
    });
  });

  it("decodes the replay range before catch-up begins", () => {
    const wire = new Uint8Array(33);
    wire[0] = MSG_TYPE.REPLAY_START;
    wire.set(historyBytes, 1);
    new DataView(wire.buffer).setBigUint64(17, 40n, true);
    new DataView(wire.buffer).setBigUint64(25, 99n, true);
    expect(decodeMessage(wire.buffer)).toEqual({
      type: "replayStart",
      historyId: "12345678-1234-5678-9abc-def012345678",
      afterSeq: 40,
      lastSeq: 99,
    });
  });
});

describe("a replay batch", () => {
  const frame = (bytes: Uint8Array): ArrayBuffer =>
    bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;

  it("unwraps every message with its sequence, in order", () => {
    const entries = [
      { seq: 7, payload: Uint8Array.from([MSG_TYPE.UNDO_POINT, 3]) },
      { seq: 8, payload: stroke(3, { x: 4, y: 4 }) },
      { seq: 2 ** 32 + 9, payload: Uint8Array.from([MSG_TYPE.UNDO, 3, 0]) },
    ];
    const batch = unwrapReplayBatch(frame(replayBatch(entries)));
    expect(batch?.historyId).toBe(HISTORY_ID);
    expect(batch?.entries.map((entry) => entry.seq)).toEqual([7, 8, 2 ** 32 + 9]);
    expect(batch?.entries.map((entry) => Array.from(new Uint8Array(entry.payload))))
      .toEqual(entries.map((entry) => Array.from(entry.payload)));
    // What comes out is what a SEQUENCED frame would have carried.
    expect(decodeMessage(batch!.entries[1].payload)?.type).toBe("stroke");
  });

  it("is refused whole when its count and its contents disagree", () => {
    const good = replayBatch([{ seq: 1, payload: Uint8Array.from([MSG_TYPE.UNDO_POINT, 3]) }]);
    const lying = good.slice();
    new DataView(lying.buffer).setUint32(17, 2, true);
    expect(unwrapReplayBatch(frame(lying))).toBeNull();
  });

  it("is refused whole when it will not inflate or an entry is cut short", () => {
    const head = new Uint8Array(21);
    head[0] = MSG_TYPE.REPLAY_BATCH;
    head.set(uuidBytes(HISTORY_ID), 1);
    new DataView(head.buffer).setUint32(17, 1, true);
    expect(unwrapReplayBatch(frame(new Uint8Array([...head, 1, 2, 3, 4, 5])))).toBeNull();

    // A body whose one entry claims forty bytes and carries three.
    const body = new Uint8Array(12 + 3);
    new DataView(body.buffer).setBigUint64(0, 1n, true);
    new DataView(body.buffer).setUint32(8, 40, true);
    expect(unwrapReplayBatch(frame(new Uint8Array([...head, ...deflateZlib(body)])))).toBeNull();

    expect(unwrapReplayBatch(frame(new Uint8Array([MSG_TYPE.REPLAY_BATCH])))).toBeNull();
  });
});

describe("ephemeral pointer positions", () => {
  it("round-trips signed quarter-pixel canvas coordinates", () => {
    expect(decodeMessage(encodeMovePointer(ID, -12.25, 1023.75))).toEqual({
      type: "movePointer",
      userId: ID,
      x: -12.25,
      y: 1023.75,
    });
  });
});

describe("the public painter operation adapter", () => {
  it("keeps the oeee wire protocol on the consumer side", () => {
    const operation: PainterOperation = {
      kind: "line",
      layer: "foreground",
      brushSize: 7,
      brush: "halftone",
      color: COLOR,
      from: { x: 2, y: 3 },
      to: { x: 18, y: 19 },
      mask: { type: 2, r: 4, g: 5, b: 6 },
    };
    // Nothing named a target, so the mark lands in the author's own layers
    // and comes back saying so.
    expect(decodePainterOperation(decodeMessage(encodePainterOperation(ID, operation))!))
      .toEqual({ ...operation, targetActorId: String(ID) });
  });
});

/**
 * Every field has to survive the trip. A message whose buffer is a byte short
 * still encodes without complaint -- the last field just lands outside it and
 * reads back as zero -- so these assert the values rather than the lengths.
 */
describe("the tool messages", () => {
  it("round-trips a region tool with its rectangle", () => {
    const bytes = encodeRegion(ID, ID, "foreground", "blurRect",
      { x: 5, y: 7, width: 40, height: 22 }, COLOR, 9
    );
    const msg = decodeMessage(bytes);
    expect(msg).toEqual({
      type: "region",
      userId: ID,
      targetOwner: ID,
      layer: "foreground",
      tool: "blurRect",
      rect: { x: 5, y: 7, width: 40, height: 22 },
      color: COLOR,
      brushSize: 9,
      mask: NO_WIRE_MASK,
    });
  });

  it("round-trips every region tool, so no code is unreachable", () => {
    const tools: RegionTool[] = [
      "eraseRect", "blurRect", "merge", "flipH", "flipV", "turn",
      "rect", "rectFill", "ellipse", "ellipseFill", "copy", "paste",
    ];
    for (const tool of tools) {
      const bytes = encodeRegion(ID, ID, "background", tool, { x: 1, y: 2, width: 3, height: 4 }, COLOR, 1
      );
      const msg = decodeMessage(bytes);
      expect(msg && "tool" in msg && msg.tool).toBe(tool);
    }
    // and the table is the full set, so a new tool cannot be silently missed
    expect(Object.keys(REGION_TOOL)).toHaveLength(tools.length);
  });

  it("drops a region tool code it does not know rather than guessing", () => {
    const bytes = encodeRegion(ID, ID, "background", "rect", { x: 0, y: 0, width: 1, height: 1 }, COLOR, 1
    );
    // A code from some future client
    new Uint8Array(bytes)[4] = 200;
    expect(decodeMessage(bytes)).toBeNull();
  });

  it("round-trips a line, including negative coordinates", () => {
    const bytes = encodeLine(ID, ID, "background", 4, "eraser", COLOR, { x: -30, y: 8 }, { x: 200, y: -5 }
    );
    expect(decodeMessage(bytes)).toEqual({
      type: "line",
      userId: ID,
      targetOwner: ID,
      layer: "background",
      brushSize: 4,
      brushType: "eraser",
      color: COLOR,
      from: { x: -30, y: 8 },
      to: { x: 200, y: -5 },
      mask: NO_WIRE_MASK,
    });
  });

  it("round-trips a bezier's four points in NEO's order", () => {
    const points = [10, 20, 30, 40, 50, 60, 70, 80];
    const bytes = encodeBezier(ID, ID, "foreground", 6, "brush", COLOR, points);
    const msg = decodeMessage(bytes);
    expect(msg).toEqual({
      type: "bezier",
      userId: ID,
      targetOwner: ID,
      layer: "foreground",
      brushSize: 6,
      brushType: "brush",
      color: COLOR,
      points,
      mask: NO_WIRE_MASK,
    });
  });

  it("round-trips eraseAll", () => {
    expect(decodeMessage(encodeEraseAll(ID, ID, "foreground"))).toEqual({
      type: "eraseAll",
      userId: ID,
      targetOwner: ID,
      layer: "foreground",
    });
  });

  it("round-trips text, including multi-byte characters", () => {
    const text = "손글씨 🎨 tegaki";
    const bytes = encodeText(ID, ID, "background", 40, 12, text, COLOR, 14);
    expect(decodeMessage(bytes)).toEqual({
      type: "text",
      userId: ID,
      targetOwner: ID,
      layer: "background",
      x: 40,
      y: 12,
      text,
      color: COLOR,
      brushSize: 14,
      mask: NO_WIRE_MASK,
    });
    // The length prefix counts UTF-8 bytes, not characters
    expect(new Uint8Array(bytes).length).toBe(
      15 + new TextEncoder().encode(text).length + MASK_BYTES
    );
  });

  it("rejects a truncated message instead of reading past the end", () => {
    const full = encodeText(ID, ID, "background", 1, 2, "hello", COLOR, 3);
    for (const len of [0, 1, 5, 13, full.byteLength - 1]) {
      expect(decodeMessage(full.slice(0, len))).toBeNull();
    }
  });

  it("keeps the new codes clear of the existing ones", () => {
    const codes = Object.values(MSG_TYPE);
    expect(new Set(codes).size).toBe(codes.length);
  });
});

/**
 * The server asks the room who can make a checkpoint before it asks anyone to
 * make one, because making one is expensive and a client mid catch-up cannot
 * make a correct one at all. These pin the two things a client has to get
 * right about that exchange.
 */
describe("the checkpoint query", () => {
  const resetRequest = (phase?: number): ArrayBuffer => {
    const bytes = new Uint8Array(phase === undefined ? 9 : 10);
    bytes[0] = MSG_TYPE.RESET_REQUEST;
    new DataView(bytes.buffer).setBigUint64(1, 1234n, true);
    if (phase !== undefined) bytes[9] = phase;
    return bytes.buffer;
  };

  it("tells being asked apart from being told", () => {
    expect(decodeMessage(resetRequest(0))).toMatchObject({
      type: "resetRequest",
      phase: "query",
    });
    expect(decodeMessage(resetRequest(1))).toMatchObject({
      type: "resetRequest",
      phase: "upload",
    });
  });

  /**
   * Both colours of a blue/green deploy serve at once, so a client can meet a
   * server that predates the query phase. That server only ever meant "upload",
   * and a client that read the missing byte as a query would answer a question
   * nobody asked and never make the checkpoint.
   */
  it("reads a request from before the query phase as an instruction", () => {
    expect(decodeMessage(resetRequest())).toMatchObject({
      type: "resetRequest",
      phase: "upload",
    });
  });

  it("answers with nothing but the answer", () => {
    // The server knows which connection sent it; anything else in here would
    // be a claim the server would have to check.
    expect(new Uint8Array(encodeResetOffer())).toEqual(
      new Uint8Array([MSG_TYPE.RESET_OFFER]),
    );
  });

  /**
   * Every message that carries drawing state carries a mask, always. A
   * receiver that lost one would draw different pixels from the sender, so
   * these pin that it survives the trip and that a message too short to hold
   * one is rejected rather than read past.
   */
  describe("the mask", () => {
    const MASK = { type: 2, r: 18, g: 52, b: 86 };

    it("travels on every message that carries drawing state", () => {
      const cases = [
        encodeStroke(ID, ID, "foreground", 4, "solid", 1, 2, 3, 255, [{ x: 1, y: 2 }], MASK),
        encodeFill(ID, ID, "foreground", 1, 2, 3, 4, 5, 255, MASK),
        encodeRegion(ID, ID, "foreground", "blurRect", { x: 1, y: 2, width: 3, height: 4 }, COLOR, 5, MASK),
        encodeLine(ID, ID, "foreground", 4, "solid", COLOR, { x: 1, y: 2 }, { x: 3, y: 4 }, MASK),
        encodeBezier(ID, ID, "foreground", 4, "solid", COLOR, [1, 2, 3, 4, 5, 6, 7, 8], MASK),
        encodeText(ID, ID, "foreground", 1, 2, "hi", COLOR, 3, MASK),
      ];
      for (const bytes of cases) {
        expect(decodeMessage(bytes)).toMatchObject({ mask: MASK });
      }
    });

    it("reads as none when the sender was not masking", () => {
      const bytes = encodeStroke(ID, ID, "foreground", 4, "solid", 1, 2, 3, 255, [
        { x: 1, y: 2 },
      ]);
      expect(decodeMessage(bytes)).toMatchObject({ mask: NO_WIRE_MASK });
    });

    it("is found past a variable-length payload, not at a guessed offset", () => {
      const text = encodeText(ID, ID, "foreground", 1, 2, "x".repeat(300), COLOR, 3, MASK);
      expect(decodeMessage(text)).toMatchObject({ mask: MASK });

      const points = Array.from({ length: 200 }, (_, i) => ({ x: i, y: i }));
      const stroke = encodeStroke(ID, ID, "foreground", 4, "solid", 1, 2, 3, 255, points, MASK);
      expect(decodeMessage(stroke)).toMatchObject({ mask: MASK, points });
    });

    it("rejects a message whose mask was cut off", () => {
      const full = encodeStroke(ID, ID, "foreground", 4, "solid", 1, 2, 3, 255, [
        { x: 1, y: 2 },
      ]);
      // One byte short of a complete mask is not a maskless message
      expect(decodeMessage(full.slice(0, full.byteLength - 1))).toBeNull();
      expect(decodeMessage(full.slice(0, full.byteLength - MASK_BYTES))).toBeNull();
    });
  });
});

/**
 * Every kind, through the adapter the app actually calls.
 *
 * The map below is typed so that adding a kind to the vocabulary and not to
 * this test is a compile error. A fill went out to nobody for a day because it
 * was the one kind this file did not carry: the encoder had a case for it that
 * threw, which typechecks perfectly and is only wrong when it runs.
 */
const SAMPLES: Record<PainterOperation["kind"], PainterOperation> = {
  stroke: {
    kind: "stroke", layer: "background", brushSize: 1, brush: "solid",
    color: COLOR, points: [{ x: 1, y: 2 }], mask: NO_WIRE_MASK,
  },
  fill: {
    kind: "fill", layer: "background", at: { x: 1, y: 2 },
    color: COLOR, mask: NO_WIRE_MASK,
  },
  "fill-region": {
    kind: "fill-region", layer: "background", at: { x: 1, y: 2 },
    width: 3, height: 2, color: COLOR,
    coverage: new Uint8Array([0b10110000]), mask: NO_WIRE_MASK,
  },
  line: {
    kind: "line", layer: "background", brushSize: 1, brush: "solid",
    color: COLOR, from: { x: 0, y: 0 }, to: { x: 4, y: 4 }, mask: NO_WIRE_MASK,
  },
  bezier: {
    kind: "bezier", layer: "background", brushSize: 1, brush: "solid",
    color: COLOR, points: [0, 1, 2, 3, 4, 5, 6, 7], mask: NO_WIRE_MASK,
  },
  region: {
    kind: "region", layer: "background", tool: "rect",
    rect: { x: 1, y: 2, width: 3, height: 4 }, color: COLOR,
    brushSize: 1, mask: NO_WIRE_MASK,
  },
  text: {
    kind: "text", layer: "background", at: { x: 1, y: 2 }, text: "hi",
    color: COLOR, brushSize: 4, mask: NO_WIRE_MASK,
  },
  "clear-layer": { kind: "clear-layer", layer: "background" },
  "undo-boundary": { kind: "undo-boundary" },
  undo: { kind: "undo", redo: false },
};

describe("every operation the vocabulary has", () => {
  it("survives the trip out and back", () => {
    for (const [kind, operation] of Object.entries(SAMPLES)) {
      const bytes = encodePainterOperation(ID, operation);
      const decoded = decodeMessage(bytes);
      expect(decoded, `${kind} did not decode`).not.toBeNull();
      const back = decodePainterOperation(decoded!);
      expect(back?.kind, `${kind} came back as something else`).toBe(kind);

      // And is recognised as history. A kind the client does not route is
      // still sequenced by the server, so every client waits forever for a
      // message that was delivered and never applied, then drops the
      // connection believing it has fallen behind.
      expect(
        isCanvasHistoryMessage(decoded!),
        `${kind} would not reach the canvas history`,
      ).toBe(true);
    }
  });
});

describe("a flood fill on the wire", () => {
  it("encodes through the same door as everything else", () => {
    // It was encoded by a different function for a while, and the adapter
    // every caller actually uses threw instead. The fill was made, the send
    // failed, and the only screen it reached was the one that drew it.
    const operation: PainterOperation = {
      kind: "fill-region",
      layer: "background",
      targetActorId: "7",
      at: { x: 5, y: 6 },
      width: 3,
      height: 2,
      color: { r: 200, g: 100, b: 50, a: 255 },
      coverage: new Uint8Array([0b10110000]),
      mask: NO_WIRE_MASK,
    };

    const bytes = encodePainterOperation(ID, operation);
    const decoded = decodeMessage(bytes);
    expect(decoded).toMatchObject({
      type: "putImage",
      userId: ID,
      targetOwner: 7,
      layer: "background",
      x: 5,
      y: 6,
      width: 3,
      height: 2,
      color: { r: 200, g: 100, b: 50, a: 255 },
    });

    const back = decodePainterOperation(decoded!);
    expect(back).toMatchObject({
      kind: "fill-region",
      targetActorId: "7",
      at: { x: 5, y: 6 },
      width: 3,
      height: 2,
      color: { r: 200, g: 100, b: 50, a: 255 },
    });
    expect(Array.from((back as { coverage: Uint8Array }).coverage)).toEqual([0b10110000]);
  });
});
