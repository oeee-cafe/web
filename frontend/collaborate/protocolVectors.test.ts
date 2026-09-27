import { describe, expect, it } from "vitest";
import {
  decodeMessage,
  encodeChat,
  encodeEndSession,
  encodeResetBegin,
  unwrapSequenced,
} from "./binaryProtocol";
import vectors from "./test/protocolVectors.json";

/**
 * The client's half of test/protocolVectors.json; the server's is
 * src/web/handlers/collaborate/protocol_vectors.rs. Each side used to be
 * tested only against frames it built itself, so a layout both got wrong the
 * same way, or one side changed without the other, passed both.
 */

function bytesOf(hex: string): ArrayBuffer {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out.buffer;
}

function hexOf(buffer: ArrayBuffer): string {
  return Array.from(new Uint8Array(buffer), (b) => b.toString(16).padStart(2, "0")).join("");
}

describe("the frames the server writes", () => {
  it.each(Object.entries(vectors.server))("decodes %s", (_name, entry) => {
    expect(decodeMessage(bytesOf(entry.hex))).toEqual(entry.decoded);
  });

  it("unwraps a sequenced envelope to the frame inside it", () => {
    const envelope = vectors.sequenced;
    const unwrapped = unwrapSequenced(bytesOf(envelope.hex));
    expect(unwrapped).not.toBeNull();
    expect(unwrapped!.historyId).toBe(envelope.historyId);
    expect(unwrapped!.seq).toBe(envelope.seq);
    const inner = vectors.server[envelope.payload as keyof typeof vectors.server];
    expect(hexOf(unwrapped!.payload)).toBe(inner.hex);
  });
});

describe("the frames the client writes", () => {
  it("encodes chat", () => {
    const { hex, fields } = vectors.client.chat;
    expect(hexOf(encodeChat(fields.userId, fields.message, fields.timestamp))).toBe(hex);
  });

  it("encodes reset begin", () => {
    const { hex, fields } = vectors.client.resetBegin;
    expect(hexOf(encodeResetBegin(fields.lastSeq, fields.count))).toBe(hex);
  });

  it("encodes end session", () => {
    const { hex, fields } = vectors.client.endSession;
    expect(hexOf(encodeEndSession(fields.userId, fields.postUrl))).toBe(hex);
  });
});
