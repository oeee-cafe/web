import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nProvider } from "@lingui/react";
import { i18n } from "@lingui/core";
import App from "./App";
import { DefaultI18n } from "neo-cucumber";
import { setupI18n } from "./i18n";
import { encodePainterOperation, MSG_TYPE } from "./binaryProtocol";
import type { PainterOperation } from "neo-cucumber";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * A join replays history; the canvas that comes out of it has to be the one
 * everybody else is looking at.
 *
 * This drives the real session view -- the real painter, the real drain, the
 * real decoder -- from the frames a server would send, because every part of
 * that path had tests and the failure lived between them. A room checkpoints
 * roughly every five hundred messages, so a full replay begins with somebody
 * else's snapshots and a RESET_POINT that arrives after them, and what those
 * two do to each other is not reachable from a test of either one.
 *
 * The shape is taken from a session that lost its canvas in production
 * (c9b8321d, 2026-08-20): four snapshots at the checkpoint sequence, a handful
 * of operations sequenced after the base while the upload was in flight, the
 * RESET_POINT, then ordinary drawing. The artwork is not -- these marks are
 * rectangles at known coordinates so a missing layer is an assertion rather
 * than something somebody has to notice.
 */

const SESSION = "c9b8321d-9ae7-4872-959f-4ec6b3881197";
const HISTORY_ID = "70cad697-02d0-4f64-b7ae-a91bbbbf3404";
const WIDTH = 64;
const HEIGHT = 48;

/** The checkpoint's base, and the sequence its RESET_POINT lands on. */
const BASE_SEQ = 100;
const RESET_POINT_SEQ = 105;

/** Where each participant's work sits, so "whose layer went missing" is a
 * question about pixels rather than about bookkeeping. */
const MARK = {
  ownerOneSnapshot: { x: 4, y: 4 },
  ownerThreeSnapshot: { x: 30, y: 4 },
  ownerOneAfterBase: { x: 4, y: 24 },
  ownerTwoAfterReset: { x: 30, y: 36 },
};

let host: HTMLElement | null = null;
let root: Root | null = null;
let sockets: FakeSocket[] = [];
let RealWebSocket: typeof WebSocket;
let realFetch: typeof fetch;

class FakeSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  readyState = FakeSocket.CONNECTING;
  binaryType = "blob";
  onopen: (() => void) | null = null;
  onclose: ((event: { code: number; reason: string; wasClean: boolean }) => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  readonly sent: ArrayBuffer[] = [];
  readonly url: string;

  constructor(url: string) {
    this.url = url;
    sockets.push(this);
  }

  send(data: ArrayBuffer) {
    this.sent.push(data);
  }

  closedWith: number | null = null;

  close(code?: number) {
    this.readyState = FakeSocket.CLOSED;
    if (this.closedWith === null && code !== undefined) this.closedWith = code;
  }

  open() {
    this.readyState = FakeSocket.OPEN;
    this.onopen?.();
  }

  /** One frame from the server, as the browser hands it over. */
  deliver(bytes: Uint8Array) {
    this.onmessage?.({ data: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) } as MessageEvent);
  }
}

function uuidBytes(uuid: string): Uint8Array {
  return Uint8Array.from(uuid.replace(/-/g, "").match(/../g)!.map((b) => parseInt(b, 16)));
}

function u64(value: number): Uint8Array {
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigUint64(0, BigInt(value), true);
  return out;
}

function concat(...parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const out = new Uint8Array(total);
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

const welcome = (sessionId: number) => Uint8Array.from([MSG_TYPE.WELCOME, sessionId]);

const replayStart = (afterSeq: number, lastSeq: number) =>
  concat(Uint8Array.from([MSG_TYPE.REPLAY_START]), uuidBytes(HISTORY_ID), u64(afterSeq), u64(lastSeq));

const caughtUp = (lastSeq: number) =>
  concat(Uint8Array.from([MSG_TYPE.CAUGHT_UP]), uuidBytes(HISTORY_ID), u64(lastSeq));

const sequenced = (seq: number, payload: Uint8Array) =>
  concat(Uint8Array.from([MSG_TYPE.SEQUENCED]), uuidBytes(HISTORY_ID), u64(seq), payload);

const resetPoint = (baseSeq: number, count: number) => {
  const out = new Uint8Array(11);
  out[0] = MSG_TYPE.RESET_POINT;
  out.set(u64(baseSeq), 1);
  new DataView(out.buffer).setUint16(9, count, true);
  return out;
};

const snapshot = (uploader: number, subject: number, layer: "background" | "foreground", png: Uint8Array) => {
  const head = new Uint8Array(8);
  head[0] = MSG_TYPE.SNAPSHOT;
  head[1] = uploader;
  head[2] = subject;
  // LAYER.FOREGROUND is 0; see binaryProtocol.
  head[3] = layer === "foreground" ? 0 : 1;
  new DataView(head.buffer).setUint32(4, png.length, true);
  return concat(head, png);
};

/** A PNG of one layer carrying a single solid square, as a checkpoint would. */
async function layerPng(mark: { x: number; y: number } | null): Promise<Uint8Array> {
  const canvas = document.createElement("canvas");
  canvas.width = WIDTH;
  canvas.height = HEIGHT;
  const context = canvas.getContext("2d")!;
  if (mark) {
    context.fillStyle = "rgb(0,0,0)";
    context.fillRect(mark.x, mark.y, 6, 6);
  }
  const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/png"));
  return new Uint8Array(await blob!.arrayBuffer());
}

/** A short opaque stroke, encoded exactly as a client would send one. */
function stroke(userId: number, at: { x: number; y: number }): Uint8Array {
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

/**
 * Whether any mounted layer has ink in the box a mark occupies.
 *
 * The union across participants on purpose: this asks whether the mark is on
 * the canvas at all, which is the question a person looking at the drawing
 * would ask, and it does not care which pair the engine put it in.
 */
function inkAt(point: { x: number; y: number }): boolean {
  const x0 = Math.max(0, point.x - 4);
  const y0 = Math.max(0, point.y - 4);
  const width = Math.min(WIDTH - x0, 14);
  const height = Math.min(HEIGHT - y0, 14);
  for (const canvas of document.querySelectorAll("canvas")) {
    if (canvas.width !== WIDTH || canvas.height !== HEIGHT) continue;
    const context = canvas.getContext("2d");
    if (!context) continue;
    const data = context.getImageData(x0, y0, width, height).data;
    for (let i = 3; i < data.length; i += 4) if (data[i] > 0) return true;
  }
  return false;
}

async function settle(times = 6) {
  for (let index = 0; index < times; index++) {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
  }
}

beforeEach(() => {
  sockets = [];
  RealWebSocket = globalThis.WebSocket;
  realFetch = globalThis.fetch;
  (globalThis as { WebSocket: unknown }).WebSocket = FakeSocket;
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.startsWith("/api/auth")) {
      return new Response(
        JSON.stringify({ user_id: "umu-uuid", login_name: "umu", preferred_locale: "en" }),
        { status: 200 },
      );
    }
    if (url.includes("/meta")) {
      return new Response(
        JSON.stringify({
          title: "", width: WIDTH, height: HEIGHT,
          ownerId: "oeee-uuid", ownerLoginName: "oeee",
          savedPostId: null, maxUsers: 8, currentUserCount: 1,
        }),
        { status: 200 },
      );
    }
    // The preview claim: never won, so the uploader stays out of the way.
    return new Response(null, { status: 409 });
  }) as typeof fetch;
  window.history.replaceState(null, "", `/collaborate/${SESSION}`);
});

afterEach(() => {
  act(() => root?.unmount());
  root = null;
  host?.remove();
  host = null;
  globalThis.WebSocket = RealWebSocket;
  globalThis.fetch = realFetch;
});

describe("a full replay that begins at a checkpoint", () => {
  it("puts the checkpoint's canvas back before the operations sequenced after it", async () => {
    setupI18n("en");
    host = document.createElement("div");
    document.body.appendChild(host);
    await act(async () => {
      root = createRoot(host!);
      root.render(
        <I18nProvider i18n={i18n} defaultComponent={DefaultI18n}>
          <App />
        </I18nProvider>,
      );
    });
    await settle();

    const socket = sockets[0];
    expect(socket, "the session view opened a socket").toBeTruthy();
    await act(async () => socket.open());
    await settle();

    // This client is umu, session id 3 -- the same id the production session
    // gave the participant whose canvas came back empty.
    await act(async () => socket.deliver(welcome(3)));
    await settle();

    const [ownerOneFg, ownerThreeFg, blank] = await Promise.all([
      layerPng(MARK.ownerOneSnapshot),
      layerPng(MARK.ownerThreeSnapshot),
      layerPng(null),
    ]);

    await act(async () => {
      socket.deliver(replayStart(0, RESET_POINT_SEQ + 2));

      // The checkpoint: a pair per participant, all at the base sequence.
      socket.deliver(sequenced(BASE_SEQ, snapshot(3, 1, "foreground", ownerOneFg)));
      socket.deliver(sequenced(BASE_SEQ, snapshot(3, 1, "background", blank)));
      socket.deliver(sequenced(BASE_SEQ, snapshot(3, 3, "foreground", ownerThreeFg)));
      socket.deliver(sequenced(BASE_SEQ, snapshot(3, 3, "background", blank)));

      // Sequenced after the base while the upload was in flight, so history
      // keeps them and the replay has to apply them on top of the snapshots.
      socket.deliver(sequenced(BASE_SEQ + 1, stroke(1, MARK.ownerOneAfterBase)));

      // The point itself, which is what tells a replaying client how many
      // snapshots the checkpoint had.
      socket.deliver(sequenced(RESET_POINT_SEQ, resetPoint(BASE_SEQ, 4)));

      // Ordinary drawing afterwards.
      socket.deliver(sequenced(RESET_POINT_SEQ + 1, stroke(2, MARK.ownerTwoAfterReset)));
      socket.deliver(sequenced(RESET_POINT_SEQ + 2, stroke(2, { x: MARK.ownerTwoAfterReset.x + 6, y: MARK.ownerTwoAfterReset.y })));
      socket.deliver(caughtUp(RESET_POINT_SEQ + 2));
    });
    await settle(12);

    // Drawing after the checkpoint is the easy half, and it working is what
    // makes a client believe the replay succeeded.
    expect(inkAt(MARK.ownerTwoAfterReset), "work sequenced after the reset point").toBe(true);

    // The checkpoint itself, which is every stroke the room made before it.
    expect(inkAt(MARK.ownerOneSnapshot), "another participant's checkpointed layer").toBe(true);
    expect(inkAt(MARK.ownerThreeSnapshot), "this client's own checkpointed layer").toBe(true);

    // And the operations that raced in after the base was chosen.
    expect(inkAt(MARK.ownerOneAfterBase), "work sequenced after the checkpoint base").toBe(true);
  });

  /**
   * The other half of the same failure, and the reason it went unnoticed.
   *
   * If the snapshots cannot be assembled the canvas has no way to hold what
   * the checkpoint stands for, and the only safe thing is to stop. Stepping
   * the canonical position over the base anyway leaves a client drawing on
   * against a canvas missing everything below it, reporting itself caught up
   * the whole time -- and reporting itself caught up is what lets it be handed
   * the *next* checkpoint to upload, which is how one client's incomplete
   * canvas becomes the room's history.
   */
  it("asks for the replay again rather than drawing on past a checkpoint it could not apply", async () => {
    setupI18n("en");
    host = document.createElement("div");
    document.body.appendChild(host);
    await act(async () => {
      root = createRoot(host!);
      root.render(
        <I18nProvider i18n={i18n} defaultComponent={DefaultI18n}>
          <App />
        </I18nProvider>,
      );
    });
    await settle();

    const socket = sockets[0];
    await act(async () => socket.open());
    await settle();
    await act(async () => socket.deliver(welcome(3)));
    await settle();

    const [ownerOneFg, blank] = await Promise.all([
      layerPng(MARK.ownerOneSnapshot),
      layerPng(null),
    ]);

    await act(async () => {
      socket.deliver(replayStart(0, RESET_POINT_SEQ + 1));
      // Two of the four the point announces: a checkpoint that cannot be made
      // whole, however long the drain waits for the rest.
      socket.deliver(sequenced(BASE_SEQ, snapshot(3, 1, "foreground", ownerOneFg)));
      socket.deliver(sequenced(BASE_SEQ, snapshot(3, 1, "background", blank)));
      socket.deliver(sequenced(RESET_POINT_SEQ, resetPoint(BASE_SEQ, 4)));
      socket.deliver(sequenced(RESET_POINT_SEQ + 1, stroke(2, MARK.ownerTwoAfterReset)));
      socket.deliver(caughtUp(RESET_POINT_SEQ + 1));
    });
    await settle(12);

    // Nothing after the checkpoint is drawn, because the position never moved
    // past it.
    expect(inkAt(MARK.ownerTwoAfterReset), "work drawn on a canvas missing its checkpoint").toBe(false);
    // And the gap is what closes the socket, which is what asks for the
    // history again.
    expect(socket.closedWith, "hung up on the canonical gap").toBe(4000);
  });
});
