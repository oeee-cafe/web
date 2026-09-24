import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { decodeMessage, unwrapReplayBatch, unwrapSequenced } from "./binaryProtocol";
import {
  FakeServer, type FakeSocket, inkAt, installRoom, mountSession,
  pointer, pressUndo, settle, settleUntil, sockets, uninstallRoom,
} from "./test/fakeRoom";
import { HISTORY_ID, stroke } from "./test/frames";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * A session, end to end, through the code that ships.
 *
 * Join, draw, be echoed, undo, see somebody else draw, lose the socket, come
 * back. Each of those has a test of its own layer -- the emitter, the
 * decoder, the canvas history, the hook's backoff -- and the failures this
 * page has had were all between two layers: a stroke stamped with one
 * identity and echoed under another, a resume that asked for the wrong
 * position, a canvas blanked on a reconnect that should have kept it. This
 * drives the real page with the server played by `FakeServer`, so those
 * seams are on the path.
 */

/** Marks in drawing coordinates, apart enough that ink at one is not ink at another. */
const MARK = {
  mine: { x: 10, y: 10 },
  theirs: { x: 40, y: 30 },
  afterReconnect: { x: 10, y: 34 },
};

/** The kinds of the canvas messages this socket sent, in order. */
function sentKinds(socket: FakeSocket): string[] {
  return socket.sent
    .map((frame) => decodeMessage(frame))
    .flatMap((message) => (message ? [message.type] : []));
}

/** The sequences delivered on this socket inside a SEQUENCED envelope. */
function deliveredSequences(socket: FakeSocket): number[] {
  return socket.delivered
    .map((frame) => unwrapSequenced(new Uint8Array(frame).buffer as ArrayBuffer))
    .flatMap((sequenced) => (sequenced ? [sequenced.seq] : []));
}

/** Every sequence delivered, whether framed on its own or inside a batch. */
function replayedSequences(socket: FakeSocket): number[] {
  return socket.delivered.flatMap((frame) => {
    const buffer = new Uint8Array(frame).buffer as ArrayBuffer;
    const batch = unwrapReplayBatch(buffer);
    if (batch) return batch.entries.map((entry) => entry.seq);
    const sequenced = unwrapSequenced(buffer);
    return sequenced ? [sequenced.seq] : [];
  });
}

let server: FakeServer;

beforeEach(() => {
  installRoom();
  server = new FakeServer();
});

afterEach(() => {
  uninstallRoom();
});

describe("a session, joined and drawn in", () => {
  it("sends a stroke under the id the server gave it, and the echo settles it in place", async () => {
    const socket = await mountSession();
    await act(async () => server.admit(socket, 3));
    await settle();

    await pointer().drag(MARK.mine, { x: MARK.mine.x + 12, y: MARK.mine.y });
    await settleUntil(() => sentKinds(socket).includes("stroke"));

    // The optimistic fork already drew it.
    expect(inkAt(MARK.mine), "the stroke, ahead of the server").toBe(true);

    // Every canvas message carries the session id WELCOME assigned, not a
    // placeholder: a stroke under one id echoed under another is a fork that
    // never settles, and a second person on the canvas who does not exist.
    const canvasMessages = socket.sent
      .map((frame) => decodeMessage(frame))
      .filter((message) => message && "userId" in message && typeof message.userId === "number");
    expect(canvasMessages.length).toBeGreaterThan(0);
    for (const message of canvasMessages) {
      expect((message as { userId: number }).userId).toBe(3);
    }
    // A stroke begins at an undo boundary, then its chunks.
    const kinds = sentKinds(socket);
    expect(kinds.indexOf("undoPoint")).toBeLessThan(kinds.indexOf("stroke"));

    // The server sequenced everything it was sent, and the echoes came back.
    await settleUntil(() => deliveredSequences(socket).length >= server.entries.length);
    expect(deliveredSequences(socket)).toEqual(server.entries.map((entry) => entry.seq));
    // The DOM canvases are uploaded on animation frames, so a look at them is
    // a bounded wait rather than an instant read.
    await settleUntil(() => inkAt(MARK.mine));
    expect(inkAt(MARK.mine), "the stroke, confirmed").toBe(true);
  });

  it("undoes through the room: one Ctrl+Z is one UNDO on the wire, and the echo clears the mark", async () => {
    const socket = await mountSession();
    await act(async () => server.admit(socket, 3));
    await settle();

    await pointer().drag(MARK.mine, { x: MARK.mine.x + 12, y: MARK.mine.y });
    await settleUntil(() => deliveredSequences(socket).length >= server.entries.length && server.entries.length > 0);
    expect(inkAt(MARK.mine)).toBe(true);

    await pressUndo();
    await settleUntil(() => sentKinds(socket).includes("undo"));
    expect(sentKinds(socket).filter((kind) => kind === "undo")).toHaveLength(1);

    await settleUntil(() => deliveredSequences(socket).length >= server.entries.length);
    await settleUntil(() => !inkAt(MARK.mine));
    expect(inkAt(MARK.mine), "the stroke, after its undo was echoed").toBe(false);
  });

  it("puts another participant's stroke on the canvas while drawing is live", async () => {
    const socket = await mountSession();
    await act(async () => server.admit(socket, 3));
    await settle();

    expect(inkAt(MARK.theirs)).toBe(false);
    await act(async () => { server.sequence(stroke(2, MARK.theirs)); });
    await settleUntil(() => inkAt(MARK.theirs));
    expect(inkAt(MARK.theirs)).toBe(true);
  });
});

describe("a join into a room with history", () => {
  it("takes the replay as one batch and comes out at the room's canvas", async () => {
    server.sequence(stroke(1, MARK.theirs));
    server.sequence(stroke(2, MARK.mine));
    const socket = await mountSession();
    await act(async () => server.admit(socket, 3));
    await settle(12);

    // One frame carried the whole history.
    expect(socket.delivered.filter((frame) => frame[0] === 0x10)).toHaveLength(1);
    expect(deliveredSequences(socket), "no message framed on its own").toEqual([]);
    await settleUntil(() => inkAt(MARK.theirs) && inkAt(MARK.mine));
    // And drawing is live at the right position.
    await pointer().drag(MARK.afterReconnect, { x: MARK.afterReconnect.x + 12, y: MARK.afterReconnect.y });
    await settleUntil(() => deliveredSequences(socket).length > 0);
    expect(deliveredSequences(socket)[0]).toBe(3);
  });
});

describe("a session whose socket drops", () => {
  it("comes back on its own, resumes from the last applied sequence, and keeps the canvas", async () => {
    const socket = await mountSession();
    await act(async () => server.admit(socket, 3));
    await settle();

    await pointer().drag(MARK.mine, { x: MARK.mine.x + 12, y: MARK.mine.y });
    await settleUntil(() => server.entries.length > 0 && deliveredSequences(socket).length >= server.entries.length);
    const applied = server.lastSeq;

    // The server goes away: a redeploy, as the browser reports it.
    await act(async () => server.hangUp(socket));
    // The hook's own backoff opens the next socket; nobody clicks anything.
    await settleUntil(() => sockets.length === 2, 100);
    const next = sockets[1];

    // It asks to continue where the canvas is, on the history it was on.
    const url = new URL(next.url);
    expect(url.searchParams.get("history_id")).toBe(HISTORY_ID);
    expect(url.searchParams.get("after_seq")).toBe(String(applied));

    await act(async () => server.admit(next, 3));
    await settle();

    // Nothing was replayed, because nothing was missed...
    expect(deliveredSequences(next)).toEqual([]);
    // ...and the canvas was kept rather than rebuilt.
    expect(inkAt(MARK.mine), "the stroke, across the reconnect").toBe(true);

    // Drawing is live again, on the same history, at the next sequence.
    await pointer().drag(MARK.afterReconnect, { x: MARK.afterReconnect.x + 12, y: MARK.afterReconnect.y });
    await settleUntil(() => sentKinds(next).includes("stroke"));
    await settleUntil(() => deliveredSequences(next).length >= server.lastSeq - applied);
    expect(deliveredSequences(next)[0]).toBe(applied + 1);
    await settleUntil(() => inkAt(MARK.afterReconnect));
    expect(inkAt(MARK.afterReconnect), "a stroke after the reconnect").toBe(true);
  });

  it("rebuilds the canvas from a history it has never seen when its resume position is refused", async () => {
    const socket = await mountSession();
    await act(async () => server.admit(socket, 3));
    await settle();

    await pointer().drag(MARK.mine, { x: MARK.mine.x + 12, y: MARK.mine.y });
    await settleUntil(() => server.entries.length > 0 && deliveredSequences(socket).length >= server.entries.length);
    expect(inkAt(MARK.mine)).toBe(true);

    // While this client was away the room checkpointed and its history was
    // replaced: a different identity, holding somebody else's stroke and not
    // this client's.
    server.replaceHistory("1a2b3c4d-0000-4000-8000-000000000001", [
      { seq: 1, payload: stroke(2, MARK.theirs) },
    ]);
    await act(async () => server.hangUp(socket));
    await settleUntil(() => sockets.length === 2, 100);
    const next = sockets[1];
    expect(new URL(next.url).searchParams.get("history_id")).toBe(HISTORY_ID);

    await act(async () => server.admit(next, 3));
    await settle(12);

    // The refused position means a full replay of the new history, onto a
    // canvas cleared of what the old one held.
    expect(replayedSequences(next)).toEqual([1]);
    expect(inkAt(MARK.theirs), "the new history's stroke").toBe(true);
    expect(inkAt(MARK.mine), "a stroke the new history never had").toBe(false);

    // And the next stroke is sequenced on the new history.
    await pointer().drag(MARK.afterReconnect, { x: MARK.afterReconnect.x + 12, y: MARK.afterReconnect.y });
    await settleUntil(() => sentKinds(next).includes("stroke"));
    await settleUntil(() => replayedSequences(next).length >= server.entries.length);
    expect(server.lastSeq).toBeGreaterThan(1);
    await settleUntil(() => inkAt(MARK.afterReconnect));
    expect(inkAt(MARK.afterReconnect)).toBe(true);
  });

  it("stays down when the server refuses the join, rather than retrying a session that is over", async () => {
    const socket = await mountSession();
    await act(async () => socket.hangUp(1008));
    await settle(60);
    expect(sockets.length, "no retry after a policy close").toBe(1);
  });
});
