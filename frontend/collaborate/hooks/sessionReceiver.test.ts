import { describe, expect, it } from "vitest";
import { SessionReceiver, type ReceiverHandlers, type ReceiverSocket } from "./sessionReceiver";
import type { SessionLink, SessionView } from "./useWebSocket";
import {
  caughtUp, concat, HISTORY_ID, replayBatch, replayStart, sequenced, stroke, u64, uuidBytes,
  welcome,
} from "../test/frames";
import { encodeEndSession, encodeMovePointer, encodePointerUp, MSG_TYPE } from "../binaryProtocol";

/**
 * The frames of a session, handed to the receiver one at a time, with the
 * socket and the page both played by the test.
 *
 * What is asserted is the receiver's decisions: which handler a frame
 * reaches, what the link says afterwards, when the socket is closed and
 * why, and what the page is told. The reconnect decisions in particular --
 * a resume the server accepted, one it refused, a history that changed
 * under the client -- used to be reachable only by mounting the page.
 */

const OTHER_HISTORY = "1a2b3c4d-0000-4000-8000-000000000001";

const frame = (bytes: Uint8Array): ArrayBuffer =>
  bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;

/** LAYERS with one participant, as the server spells it. */
const layers = (userId: string, sessionId: number, name: string) => {
  const nameBytes = new TextEncoder().encode(name);
  const head = new Uint8Array(3);
  head[0] = MSG_TYPE.LAYERS;
  new DataView(head.buffer).setUint16(1, 1, true);
  const nameLength = new Uint8Array(2);
  new DataView(nameLength.buffer).setUint16(0, nameBytes.length, true);
  return concat(
    head, uuidBytes(userId), Uint8Array.from([sessionId]), nameLength, nameBytes, u64(1000),
  );
};

const sessionExpired = () =>
  concat(Uint8Array.from([MSG_TYPE.SESSION_EXPIRED]), uuidBytes("c9b8321d-9ae7-4872-959f-4ec6b3881197"));

class Page {
  readonly link: SessionLink = {
    connection: "connecting", catchingUp: true, localId: null, lastSeq: 0, shouldConnect: true,
  };
  readonly published: Partial<SessionView>[] = [];
  readonly closed: { code: number; reason: string }[] = [];
  readonly socket: ReceiverSocket = {
    close: (code, reason) => { this.closed.push({ code, reason }); },
  };
  /** Every handler call, as `name(args)`. */
  readonly calls: string[] = [];
  /** What `verifyCanonicalPosition` answers. */
  verified = true;
  readonly handlers: ReceiverHandlers;
  readonly receiver: SessionReceiver;
  clock = 0;

  constructor() {
    const note = (name: string) => (...args: unknown[]) => {
      this.calls.push(`${name}(${args.map((arg) => JSON.stringify(arg)).join(",")})`);
    };
    this.handlers = {
      onSynchronizationError: note("onSynchronizationError"),
      createOrUpdateCursor: note("createOrUpdateCursor"),
      hideCursor: note("hideCursor"),
      addParticipant: note("addParticipant"),
      clearParticipants: note("clearParticipants"),
      addChatMessage: note("addChatMessage"),
      onResetQuery: note("onResetQuery"),
      onResetSelected: note("onResetSelected"),
      onReconnectCanvas: note("onReconnectCanvas"),
      onCanvasMessage: async (message, _raw, sequence) => {
        this.calls.push(`onCanvasMessage(${message.type},${sequence})`);
      },
      onUnreadableSequence: async (sequence) => { this.calls.push(`onUnreadableSequence(${sequence})`); },
      onWelcome: note("onWelcome"),
      onResetPoint: note("onResetPoint"),
      verifyCanonicalPosition: async () => this.verified,
      onSessionEnded: note("onSessionEnded"),
      onSessionExpired: note("onSessionExpired"),
    };
    this.receiver = new SessionReceiver(
      this.link,
      (change) => {
        if (change.connection !== undefined) this.link.connection = change.connection;
        if (change.catchingUp !== undefined) this.link.catchingUp = change.catchingUp;
        this.published.push(change);
      },
      () => this.handlers,
      () => this.clock,
    );
  }

  /** Frames from the server, in order. */
  async receive(...frames: Uint8Array[]) {
    for (const bytes of frames) await this.receiver.receive(this.socket, frame(bytes));
  }

  /** A first connection, through welcome, an empty replay and catch-up. */
  async join(sessionId = 3, lastSeq = 0) {
    await this.receiver.opened(false);
    await this.receive(welcome(sessionId), replayStart(0, lastSeq));
  }

  calledWith(prefix: string) {
    return this.calls.filter((call) => call.startsWith(prefix));
  }
}

describe("a first connection", () => {
  it("takes the session id from WELCOME and holds drawing until CAUGHT_UP is verified", async () => {
    const page = new Page();
    await page.receiver.opened(false);
    expect(page.calledWith("onReconnectCanvas")).toEqual(["onReconnectCanvas(false,null)"]);
    expect(page.link.catchingUp).toBe(true);

    await page.receive(welcome(3));
    expect(page.link.localId).toBe(3);
    expect(page.calledWith("onWelcome")).toEqual(["onWelcome(3)"]);

    await page.receive(replayStart(0, 2));
    await page.receive(sequenced(1, stroke(1, { x: 4, y: 4 })), sequenced(2, stroke(2, { x: 8, y: 4 })));
    expect(page.calledWith("onCanvasMessage")).toEqual(["onCanvasMessage(stroke,1)", "onCanvasMessage(stroke,2)"]);
    expect(page.link.lastSeq).toBe(2);
    expect(page.link.catchingUp, "not until the server says so").toBe(true);

    await page.receive(caughtUp(2));
    expect(page.link.catchingUp).toBe(false);
    expect(page.published.at(-1)).toEqual({
      catchingUp: false,
      progress: { phase: "ready", receivedSequence: 2, appliedSequence: 2, targetSequence: 2 },
    });
    expect(page.receiver.historyId).toBe(HISTORY_ID);
    expect(page.closed).toEqual([]);
  });

  it("takes a batched replay exactly as it takes the same messages framed one by one", async () => {
    const page = new Page();
    await page.join(3, 3);
    await page.receive(replayBatch([
      { seq: 1, payload: stroke(1, { x: 4, y: 4 }) },
      { seq: 2, payload: Uint8Array.from([0xfe, 1, 2]) },
      { seq: 3, payload: stroke(2, { x: 8, y: 4 }) },
    ]));
    expect(page.calledWith("onCanvasMessage")).toEqual(["onCanvasMessage(stroke,1)", "onCanvasMessage(stroke,3)"]);
    expect(page.calledWith("onUnreadableSequence")).toEqual(["onUnreadableSequence(2)"]);
    expect(page.link.lastSeq).toBe(3);
    await page.receive(caughtUp(3));
    expect(page.link.catchingUp).toBe(false);
    expect(page.closed).toEqual([]);
  });

  it("hangs up on a batch it cannot read rather than applying part of one", async () => {
    const page = new Page();
    await page.join(3, 2);
    const batch = replayBatch([{ seq: 1, payload: stroke(1, { x: 4, y: 4 }) }]);
    new DataView(batch.buffer).setUint32(17, 5, true);
    await page.receive(batch);
    expect(page.calledWith("onCanvasMessage")).toEqual([]);
    expect(page.closed).toEqual([{ code: 4000, reason: "unreadable replay batch" }]);
  });

  it("hangs up on a gap the server's CAUGHT_UP would otherwise paper over", async () => {
    const page = new Page();
    await page.join(3, 2);
    page.verified = false;
    await page.receive(sequenced(2, stroke(1, { x: 4, y: 4 })), caughtUp(2));
    expect(page.link.catchingUp).toBe(true);
    expect(page.closed).toEqual([{ code: 4000, reason: "canonical sequence gap" }]);
  });

  it("publishes the catch-up position no more than once per throttle", async () => {
    const page = new Page();
    await page.join(3, 50);
    const before = page.published.length;
    for (let seq = 1; seq <= 10; seq++) {
      page.clock += 20;
      await page.receive(sequenced(seq, stroke(1, { x: 4, y: 4 })));
    }
    const applying = page.published.slice(before).filter((change) => change.progress?.phase === "applying");
    // Ten frames 20ms apart, published at most every 100ms: the first, then
    // the one 100ms later. The readout finishes on CAUGHT_UP's own publish.
    expect(applying.map((change) => change.progress?.appliedSequence)).toEqual([1, 6]);
  });

  it("drops pointer frames while catching up, and shows them by name once live", async () => {
    const page = new Page();
    await page.join(3, 0);
    await page.receive(layers("aaaaaaaa-0000-4000-8000-000000000002", 2, "bob"));
    await page.receive(new Uint8Array(encodeMovePointer(2, 10, 12)));
    expect(page.calledWith("createOrUpdateCursor"), "not during the replay").toEqual([]);

    await page.receive(caughtUp(0));
    await page.receive(new Uint8Array(encodeMovePointer(2, 10, 12)));
    expect(page.calledWith("createOrUpdateCursor")).toEqual(['createOrUpdateCursor("2",10,12,"bob")']);
    await page.receive(new Uint8Array(encodePointerUp(2)));
    expect(page.calledWith("hideCursor")).toEqual(['hideCursor("2")']);

    // Our own pointer, echoed by mistake, is not a cursor.
    await page.receive(new Uint8Array(encodeMovePointer(3, 1, 1)));
    expect(page.calledWith("createOrUpdateCursor")).toHaveLength(1);
  });
});

describe("a sequenced frame this client cannot read", () => {
  it("is stepped over, and still counts toward the position", async () => {
    const page = new Page();
    await page.join(3, 3);
    await page.receive(sequenced(1, stroke(1, { x: 4, y: 4 })));
    await page.receive(sequenced(2, Uint8Array.from([0xfe, 1, 2, 3])));
    expect(page.calledWith("onUnreadableSequence")).toEqual(["onUnreadableSequence(2)"]);
    expect(page.link.lastSeq).toBe(2);
    await page.receive(sequenced(3, stroke(1, { x: 4, y: 4 })), caughtUp(3));
    expect(page.link.catchingUp).toBe(false);
  });

  it("includes a control frame smuggled into history, which is never acted on", async () => {
    const page = new Page();
    await page.join(3, 1);
    await page.receive(sequenced(1, welcome(9)));
    expect(page.link.localId, "WELCOME inside the history does not renumber us").toBe(3);
    expect(page.calledWith("onUnreadableSequence")).toEqual(["onUnreadableSequence(1)"]);
    await page.receive(sequenced(2, new Uint8Array(encodeEndSession("c9b8321d-9ae7-4872-959f-4ec6b3881197", "/@x/1"))));
    expect(page.calledWith("onSessionEnded")).toEqual([]);
    expect(page.link.shouldConnect).toBe(true);
  });
});

describe("a reconnect", () => {
  /** A session that has been through one connection, at sequence 5. */
  async function established() {
    const page = new Page();
    await page.join(3, 5);
    for (let seq = 1; seq <= 5; seq++) await page.receive(sequenced(seq, stroke(1, { x: 4, y: 4 })));
    await page.receive(caughtUp(5));
    page.calls.length = 0;
    return page;
  }

  it("asks to resume from the applied position on the history it was on", async () => {
    const page = await established();
    expect(page.receiver.resumeFrom(4)).toEqual({ historyId: HISTORY_ID, afterSeq: 4 });
    expect(page.link.lastSeq, "the received position restarts from what was applied").toBe(4);
    expect(page.receiver.resumeFrom(null), "nothing to resume from").toBeNull();
  });

  it("keeps the canvas when the server continues from the position it asked for", async () => {
    const page = await established();
    page.receiver.resumeFrom(5);
    await page.receiver.opened(true);
    expect(page.calledWith("onReconnectCanvas"), "not decided until the server answers").toEqual([]);

    await page.receive(welcome(3), replayStart(5, 7));
    expect(page.calledWith("onReconnectCanvas")).toEqual(["onReconnectCanvas(true,5)"]);
    expect(page.link.lastSeq).toBe(5);
    await page.receive(sequenced(6, stroke(1, { x: 4, y: 4 })), sequenced(7, stroke(1, { x: 4, y: 4 })), caughtUp(7));
    expect(page.link.catchingUp).toBe(false);
    expect(page.link.lastSeq).toBe(7);
  });

  it("clears the canvas when the server replays from the start instead", async () => {
    const page = await established();
    page.receiver.resumeFrom(5);
    await page.receiver.opened(true);
    await page.receive(welcome(3), replayStart(0, 5));
    expect(page.calledWith("onReconnectCanvas")).toEqual(["onReconnectCanvas(true,null)"]);
    expect(page.link.lastSeq, "back to the beginning of the replay").toBe(0);
  });

  it("clears the canvas when the history is not the one it was on", async () => {
    const page = await established();
    page.receiver.resumeFrom(5);
    await page.receiver.opened(true);
    await page.receive(welcome(3), replayStart(0, 1, OTHER_HISTORY));
    expect(page.calledWith("onReconnectCanvas")).toEqual(["onReconnectCanvas(true,null)"]);
    expect(page.receiver.historyId).toBe(OTHER_HISTORY);
    await page.receive(sequenced(1, stroke(2, { x: 4, y: 4 }), OTHER_HISTORY), caughtUp(1, OTHER_HISTORY));
    expect(page.link.catchingUp).toBe(false);
    expect(page.closed).toEqual([]);
  });

  it("decides from the first sequenced frame when no REPLAY_START precedes it", async () => {
    const page = await established();
    page.receiver.resumeFrom(5);
    await page.receiver.opened(true);
    await page.receive(welcome(3), sequenced(6, stroke(1, { x: 4, y: 4 })));
    expect(page.calledWith("onReconnectCanvas")).toEqual(["onReconnectCanvas(true,5)"]);
    expect(page.calledWith("onCanvasMessage")).toEqual(["onCanvasMessage(stroke,6)"]);
  });

  it("decides from CAUGHT_UP alone when nothing was missed", async () => {
    const page = await established();
    page.receiver.resumeFrom(5);
    await page.receiver.opened(true);
    await page.receive(welcome(3), caughtUp(5));
    expect(page.calledWith("onReconnectCanvas")).toEqual(["onReconnectCanvas(true,5)"]);
    expect(page.link.catchingUp).toBe(false);
  });
});

describe("a live session", () => {
  async function live() {
    const page = new Page();
    await page.join(3, 0);
    await page.receive(caughtUp(0));
    page.calls.length = 0;
    return page;
  }

  it("hangs up when the history changes identity under it", async () => {
    const page = await live();
    await page.receive(sequenced(1, stroke(1, { x: 4, y: 4 }), OTHER_HISTORY));
    expect(page.closed).toEqual([{ code: 4000, reason: "canonical history changed" }]);
    expect(page.link.catchingUp, "drawing held until the replay").toBe(true);
    expect(page.calledWith("onCanvasMessage"), "nothing from the other history applied").toEqual([]);
  });

  it("ignores a replay batch that arrives once it is live", async () => {
    const page = await live();
    await page.receive(replayBatch([{ seq: 1, payload: stroke(1, { x: 4, y: 4 }) }]));
    expect(page.calledWith("onCanvasMessage")).toEqual([]);
    expect(page.link.lastSeq).toBe(0);
  });

  it("hangs up on a sequence gap rather than drawing past it", async () => {
    const page = await live();
    page.verified = false;
    await page.receive(sequenced(2, stroke(1, { x: 4, y: 4 })));
    expect(page.closed).toEqual([{ code: 4000, reason: "canonical sequence gap" }]);
  });

  it("stops taking frames once one has failed", async () => {
    const page = await live();
    page.receiver.fail(new Error("png decode failed"));
    expect(page.calledWith("onSynchronizationError")).toEqual(['onSynchronizationError({})']);
    expect(page.link.catchingUp).toBe(true);
    await page.receive(sequenced(1, stroke(1, { x: 4, y: 4 })));
    expect(page.calledWith("onCanvasMessage")).toEqual([]);
  });

  it("asks for no more connections once the session has ended or expired", async () => {
    const ended = await live();
    await ended.receive(new Uint8Array(encodeEndSession("c9b8321d-9ae7-4872-959f-4ec6b3881197", "/@oeee/42")));
    expect(ended.calledWith("onSessionEnded")).toEqual(['onSessionEnded("/@oeee/42")']);
    expect(ended.link.shouldConnect).toBe(false);

    const expired = await live();
    await expired.receive(sessionExpired());
    expect(expired.calledWith("onSessionExpired")).toEqual(["onSessionExpired()"]);
    expect(expired.link.shouldConnect).toBe(false);
    expect(expired.link.connection).toBe("disconnected");
  });

  it("puts a remote stroke's cursor where the stroke ended, under the name LAYERS gave", async () => {
    const page = await live();
    await page.receive(layers("aaaaaaaa-0000-4000-8000-000000000002", 2, "bob"));
    expect(page.calledWith("clearParticipants")).toHaveLength(1);
    expect(page.calledWith("addParticipant")).toEqual([
      'addParticipant("aaaaaaaa-0000-4000-8000-000000000002","bob",1000,2)',
    ]);
    await page.receive(sequenced(1, stroke(2, { x: 20, y: 30 })));
    expect(page.calledWith("createOrUpdateCursor")).toEqual(['createOrUpdateCursor("2",23,30,"bob")']);
    // Our own echo is not a cursor.
    await page.receive(sequenced(2, stroke(3, { x: 20, y: 30 })));
    expect(page.calledWith("createOrUpdateCursor")).toHaveLength(1);
  });
});
