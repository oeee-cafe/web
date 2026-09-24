import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act, useEffect, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useWebSocket } from "./hooks/useWebSocket";
import { type CollaborationMeta } from "./types";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * A render is not a reason to open a socket.
 *
 * The session view re-renders constantly -- a remote cursor, a chat line, a
 * catch-up counter -- and the callbacks the WebSocket hook closes over are
 * rebuilt with it. When the effect that opens the connection depended on the
 * identity of those callbacks, every render called it again, and every path
 * that decides *not* to reconnect ends by setting connection state, which is
 * itself a render. So the decision not to reconnect scheduled the next
 * attempt: a session that was over answered 1008 to sixty-nine joins from one
 * client in eight seconds, and the backoff between attempts never ran once.
 *
 * Both halves are asserted here, because either alone leaves the loop
 * reachable: the connector the hook hands out must keep its identity, and a
 * refusal or a pending retry must survive the renders that follow it.
 */

let host: HTMLElement | null = null;
let root: Root | null = null;
let sockets: FakeSocket[] = [];
let RealWebSocket: typeof WebSocket;

/** Enough of a WebSocket for the hook to hold: it never opens on its own. */
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

  readonly url: string;

  constructor(url: string) {
    this.url = url;
    sockets.push(this);
  }

  send() {}

  close() {
    this.readyState = FakeSocket.CLOSED;
  }

  /** The server hanging up, as the browser reports it. */
  hangUp(code: number) {
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.({ code, reason: "", wasClean: code === 1008 });
  }
}

const canvasMeta: CollaborationMeta = {
  title: "",
  width: 300,
  height: 300,
  ownerId: "owner",
  ownerLoginName: "owner",
  maxUsers: 8,
  currentUserCount: 1,
};

/**
 * The session view's connect effect, with nothing else of it.
 *
 * `addChatMessage` is written inline on purpose: it is what the page used to
 * pass, and it is the cheapest way to make every render hand the hook a
 * different callback. If the connector the hook returns is rebuilt with it,
 * the effect below re-runs and the test sees the extra sockets.
 */
function Harness({ renderRef }: { renderRef: React.RefObject<(() => void) | null> }) {
  const [, setTick] = useState(0);
  renderRef.current = () => setTick((tick) => tick + 1);

  const userIdRef = useRef<string | null>("user");
  const userLoginNameRef = useRef("user");
  const localUserJoinTimeRef = useRef(0);
  const participantsRef = useRef(new Map());
  const localIdRef = useRef<number | null>(null);
  const lastSeqRef = useRef(0);
  const shouldConnectRef = useRef(true);
  const catchupTimeoutRef = useRef<number | null>(null);
  const processingMessageRef = useRef(false);
  const isCatchingUpRef = useRef(false);

  const { connect } = useWebSocket({
    canvasMeta,
    userIdRef,
    userLoginNameRef,
    localUserJoinTimeRef,
    participantsRef,
    localIdRef,
    lastSeqRef,
    shouldConnectRef,
    catchupTimeoutRef,
    processingMessageRef,
    isCatchingUpRef,
    setConnectionState: () => {},
    setIsCatchingUp: () => {},
    setSyncProgress: () => {},
    onSynchronizationError: () => {},
    createOrUpdateCursor: () => {},
    hideCursor: () => {},
    addParticipant: () => {},
    clearParticipants: () => {},
    addChatMessage: (message) => void message,
    handleResetRequest: () => {},
    canUploadCheckpoint: () => false,
    onReconnectCanvas: () => {},
    onCanvasMessage: async () => {},
    onUnreadableSequence: async () => {},
    onWelcome: () => {},
    onResetPoint: () => {},
    verifyCanonicalPosition: async () => true,
    appliedCanonicalPosition: () => 0,
    canResumeCanonicalPosition: () => false,
    onSessionEnded: () => {},
    onSessionExpired: () => {},
  });

  useEffect(() => {
    connect();
  }, [connect]);

  return null;
}

async function mountHarness() {
  const renderRef: React.RefObject<(() => void) | null> = { current: null };
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  await act(async () => {
    root!.render(<Harness renderRef={renderRef} />);
  });
  return {
    /** Renders the session view again, the way any incoming message would. */
    async rerender(times: number) {
      for (let i = 0; i < times; i += 1) {
        await act(async () => {
          renderRef.current!();
        });
      }
    },
  };
}

beforeEach(() => {
  sockets = [];
  RealWebSocket = globalThis.WebSocket;
  globalThis.WebSocket = FakeSocket as unknown as typeof WebSocket;
});

afterEach(async () => {
  globalThis.WebSocket = RealWebSocket;
  if (root) await act(async () => root!.unmount());
  host?.remove();
  root = null;
  host = null;
});

describe("reconnect backoff", () => {
  it("opens one socket for one session, however often the view renders", async () => {
    const view = await mountHarness();
    expect(sockets).toHaveLength(1);

    await view.rerender(10);

    expect(sockets).toHaveLength(1);
  });

  it("stops for good when the server refuses the join", async () => {
    const view = await mountHarness();

    // 1008: the session is over or full, and no amount of retrying changes it.
    await act(async () => sockets[0].hangUp(1008));
    await view.rerender(10);

    expect(sockets).toHaveLength(1);
  });

  it("waits out the backoff after a dropped socket instead of retrying on render", async () => {
    const view = await mountHarness();

    // 1006: a severed connection, which is worth coming back from.
    await act(async () => sockets[0].hangUp(1006));
    await view.rerender(10);

    // The first retry is RECONNECT_BASE_MS plus jitter away. Renders in the
    // meantime must neither bring it forward nor cancel it.
    expect(sockets).toHaveLength(1);

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 1500));
    });

    expect(sockets).toHaveLength(2);
  });
});
