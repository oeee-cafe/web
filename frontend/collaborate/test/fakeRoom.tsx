import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nProvider } from "@lingui/react";
import { i18n } from "@lingui/core";
import App from "../App";
import { DefaultI18n } from "neo-cucumber";
import { setupI18n } from "../i18n";
import { MSG_TYPE } from "../binaryProtocol";
import {
  caughtUp, HEIGHT, HISTORY_ID, replayBatch, replayStart, sequenced, welcome, WIDTH,
} from "./frames";


/**
 * A room for the real session view to sit in, with the server played by the
 * test.
 *
 * The page's dependencies on the outside world are three: `fetch` for the
 * sign-in and the session's metadata, `WebSocket` for the room, and the URL
 * for which room. All three are replaced here, and nothing else is -- the
 * painter, the drain, the decoder and the hook are the ones that ship.
 * Every bug this code has had lived between two of those, each of which had
 * tests of its own.
 *
 * `FakeServer` is enough of the real one to hold a conversation: it welcomes
 * a socket, replays or resumes the history, sequences what a client sends
 * and echoes it to the room. Tests that need something stranger than that --
 * a checkpoint arriving out of order, a truncated replay -- deliver frames to
 * the socket by hand.
 */

export const SESSION = "c9b8321d-9ae7-4872-959f-4ec6b3881197";

export let sockets: FakeSocket[] = [];
let host: HTMLElement | null = null;
let root: Root | null = null;
let RealWebSocket: typeof WebSocket;
let realFetch: typeof fetch;

export class FakeSocket {
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
  /** Everything the client sent, in order. */
  readonly sent: ArrayBuffer[] = [];
  /** Everything delivered to the client, in order. */
  readonly delivered: Uint8Array[] = [];
  /** Where a send goes when a server is listening. */
  sink: ((data: ArrayBuffer) => void) | null = null;
  readonly url: string;

  constructor(url: string) {
    this.url = url;
    sockets.push(this);
  }

  send(data: ArrayBuffer) {
    this.sent.push(data);
    this.sink?.(data);
  }

  /** The code the client closed with, if it was the client that hung up. */
  closedWith: number | null = null;

  close(code?: number) {
    this.readyState = FakeSocket.CLOSED;
    if (this.closedWith === null && code !== undefined) this.closedWith = code;
  }

  open() {
    this.readyState = FakeSocket.OPEN;
    this.onopen?.();
  }

  /** The server hanging up, as the browser reports it. */
  hangUp(code: number) {
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.({ code, reason: "", wasClean: code === 1008 });
  }

  /** One frame from the server, as the browser hands it over. */
  deliver(bytes: Uint8Array) {
    if (this.readyState !== FakeSocket.OPEN) return;
    this.delivered.push(bytes);
    this.onmessage?.({
      data: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
    } as MessageEvent);
  }
}

/** A PNG of one layer carrying a single solid square, as a checkpoint would. */
export async function layerPng(mark: { x: number; y: number } | null): Promise<Uint8Array> {
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

/**
 * Whether any mounted layer has ink in the box a mark occupies.
 *
 * The union across participants on purpose: this asks whether the mark is on
 * the canvas at all, which is the question a person looking at the drawing
 * would ask, and it does not care which pair the engine put it in.
 */
export function inkAt(point: { x: number; y: number }): boolean {
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

export async function settle(times = 6) {
  for (let index = 0; index < times; index++) {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
  }
}

/** Settles until `condition` holds, or fails after `times` rounds. */
export async function settleUntil(condition: () => boolean, times = 60) {
  for (let index = 0; index < times; index++) {
    if (condition()) return;
    await settle(1);
  }
  if (!condition()) throw new Error("condition never held");
}

/**
 * The message types the server sequences into history. The rest -- pointer
 * positions, presence, chat -- are forwarded or answered, never stored.
 */
const CANVAS_TYPES = new Set<number>([
  MSG_TYPE.STROKE, MSG_TYPE.FILL, MSG_TYPE.REGION, MSG_TYPE.LINE, MSG_TYPE.BEZIER,
  MSG_TYPE.ERASE_ALL, MSG_TYPE.TEXT, MSG_TYPE.PUT_IMAGE, MSG_TYPE.SNAPSHOT,
  MSG_TYPE.UNDO_POINT, MSG_TYPE.UNDO,
]);
const POINTER_TYPES = new Set<number>([MSG_TYPE.POINTER_UP, MSG_TYPE.MOVE_POINTER]);

/**
 * The room, as far as a client can tell.
 *
 * Frames the server originates go out on the next macrotask, the way a
 * network hop would deliver them, rather than from inside the client's own
 * `send`.
 */
export class FakeServer {
  historyId: string;
  entries: { seq: number; payload: Uint8Array }[] = [];
  lastSeq = 0;
  /**
   * Whether a replay a client asks for in batches is sent that way. Off, this
   * is a server from before batches, which sends a frame per message
   * whatever the URL says.
   */
  batches: boolean;
  private readonly clients = new Map<FakeSocket, number>();

  constructor(options: { historyId?: string; batches?: boolean } = {}) {
    this.historyId = options.historyId ?? HISTORY_ID;
    this.batches = options.batches ?? true;
  }

  /**
   * Accepts a socket the page opened: welcome, then the history from where
   * the URL asks for it, if that position is on this history.
   */
  admit(socket: FakeSocket, sessionId: number) {
    this.clients.set(socket, sessionId);
    socket.sink = (data) => this.receive(socket, data);
    socket.open();
    socket.deliver(welcome(sessionId));

    const url = new URL(socket.url);
    const resumeHistory = url.searchParams.get("history_id");
    const resumeAfter = Number(url.searchParams.get("after_seq") ?? "0");
    const after =
      resumeHistory === this.historyId && resumeAfter <= this.lastSeq ? resumeAfter : 0;
    socket.deliver(replayStart(after, this.lastSeq, this.historyId));
    const replay = this.entries.filter((entry) => entry.seq > after);
    if (this.batches && url.searchParams.get("replay") === "batch") {
      if (replay.length > 0) socket.deliver(replayBatch(replay, this.historyId));
    } else {
      for (const entry of replay) socket.deliver(sequenced(entry.seq, entry.payload, this.historyId));
    }
    socket.deliver(caughtUp(this.lastSeq, this.historyId));
  }

  /** Sequences a frame as if a client had sent it, and echoes it to the room. */
  sequence(payload: Uint8Array): number {
    const seq = ++this.lastSeq;
    this.entries.push({ seq, payload });
    const frame = sequenced(seq, payload, this.historyId);
    for (const socket of this.clients.keys()) this.later(socket, frame);
    return seq;
  }

  /** The server going away under the socket: a redeploy, a lost network. */
  hangUp(socket: FakeSocket, code = 1006) {
    this.clients.delete(socket);
    socket.hangUp(code);
  }

  /** A history this client has never seen, as after a reset elsewhere. */
  replaceHistory(historyId: string, entries: { seq: number; payload: Uint8Array }[] = []) {
    this.historyId = historyId;
    this.entries = entries;
    this.lastSeq = entries.reduce((max, entry) => Math.max(max, entry.seq), 0);
  }

  private receive(from: FakeSocket, data: ArrayBuffer) {
    const bytes = new Uint8Array(data);
    if (CANVAS_TYPES.has(bytes[0])) {
      this.sequence(bytes.slice());
    } else if (POINTER_TYPES.has(bytes[0])) {
      for (const socket of this.clients.keys()) {
        if (socket !== from) this.later(socket, bytes.slice());
      }
    }
  }

  private later(socket: FakeSocket, frame: Uint8Array) {
    setTimeout(() => socket.deliver(frame), 0);
  }
}

/**
 * Puts the page's surroundings in place: the sign-in, the session's
 * metadata, the room's URL, and a `WebSocket` that is one of ours.
 */
export function installRoom(options: { ownerId?: string } = {}) {
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
          ownerId: options.ownerId ?? "oeee-uuid", ownerLoginName: "oeee",
          savedPostId: null, maxUsers: 8, currentUserCount: 1,
        }),
        { status: 200 },
      );
    }
    // The preview claim: never won, so the uploader stays out of the way.
    return new Response(null, { status: 409 });
  }) as typeof fetch;
  window.history.replaceState(null, "", `/collaborate/${SESSION}`);
}

export function uninstallRoom() {
  act(() => root?.unmount());
  root = null;
  host?.remove();
  host = null;
  globalThis.WebSocket = RealWebSocket;
  globalThis.fetch = realFetch;
}

/** Mounts the session view and returns the socket it opened. */
export async function mountSession(): Promise<FakeSocket> {
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
  if (!socket) throw new Error("the session view opened no socket");
  return socket;
}

/** A pointer on the painter's canvas, in drawing coordinates. */
export function pointer() {
  const canvas = document.querySelector("#canvas") as HTMLCanvasElement | null;
  if (!canvas) throw new Error("no painter canvas mounted");
  const box = canvas.getBoundingClientRect();
  const scale = box.width / WIDTH;
  const dispatch = (type: string, x: number, y: number) =>
    act(async () => {
      canvas.dispatchEvent(new PointerEvent(type, {
        pointerId: 1, pointerType: "mouse", button: 0,
        buttons: type === "pointerup" ? 0 : 1,
        clientX: box.left + x * scale, clientY: box.top + y * scale,
        bubbles: true, cancelable: true,
      }));
    });
  return {
    /** A horizontal stroke from `from` to `to`, at one y. */
    async drag(from: { x: number; y: number }, to: { x: number; y: number }) {
      await dispatch("pointerdown", from.x, from.y);
      await dispatch("pointermove", (from.x + to.x) / 2, (from.y + to.y) / 2);
      await dispatch("pointermove", to.x, to.y);
      await dispatch("pointerup", to.x, to.y);
    },
  };
}

export async function pressUndo() {
  await act(async () => {
    window.dispatchEvent(new KeyboardEvent("keydown", {
      key: "z", code: "KeyZ", ctrlKey: true, bubbles: true, cancelable: true,
    }));
  });
}
