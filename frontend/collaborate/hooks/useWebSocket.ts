import { useCallback, useRef, useEffect, useState } from "react";
import { encodeJoin, encodeResetOffer, type DecodedMessage } from "../binaryProtocol";
import { type CollaborationMeta } from "../types";
import { SessionReceiver, type ChatLine, type ReceiverHandlers } from "./sessionReceiver";

export type ConnectionState = "disconnected" | "connecting" | "connected";

/**
 * The connection as the session view renders it.
 *
 * Published as one value by the hook that owns the socket, and mirrored into
 * `SessionLink` in the same write, so what a callback reads synchronously
 * and what the page shows can never be two different answers. The page used
 * to keep three states and copy one of them into a ref from an effect, and
 * a message arriving between the state change and the effect read the copy.
 */
export interface SessionView {
  connection: ConnectionState;
  catchingUp: boolean;
  progress: SyncProgress;
}

/**
 * What the socket hook knows about this connection, readable at any moment.
 *
 * One object rather than a ref per field handed in by the page. The page
 * makes it with `useSessionLink` so its callbacks can read it before the
 * socket hook, which takes those callbacks, has been called; only the socket
 * hook and its receiver write to it.
 */
export interface SessionLink {
  connection: ConnectionState;
  /** Drawing is held until the replay that follows a connect is applied. */
  catchingUp: boolean;
  /** The 1-byte id the server gave this connection; null until WELCOME. */
  localId: number | null;
  /** The highest canonical sequence received so far. */
  lastSeq: number;
  /** Whether the page wants a connection at all; false once it is leaving. */
  shouldConnect: boolean;
}

export interface SyncProgress {
  phase: "joining" | "receiving" | "applying" | "ready";
  receivedSequence: number;
  appliedSequence: number;
  targetSequence: number | null;
}

// A server redeploy drops every socket at once. Come back on our own instead
// of making everyone in the room notice the modal and click Reconnect: the
// canonical history lives on the server, so a reconnect restores the canvas
// exactly. Backoff is capped well below the point where a person would give
// up and reload themselves.
const RECONNECT_MAX_ATTEMPTS = 10;
const RECONNECT_BASE_MS = 500;
const RECONNECT_MAX_MS = 10000;
// Spreads the retries of a whole room across a window instead of stampeding
// the process that just came up.
const RECONNECT_JITTER_MS = 500;
// The server's close code for a refused join (session over or full).
const WS_CLOSE_POLICY = 1008;
// How long a participant who does not own the session waits before offering to
// upload a checkpoint, giving the owner the first refusal. Long enough that the
// owner's answer wins a same-tick race, short enough that nobody notices when
// the owner is not there to take it.
const NON_OWNER_RESET_OFFER_DELAY_MS = 300;

interface WebSocketHookParams {
  /** From `useSessionLink`; written here, read anywhere on the page. */
  link: SessionLink;
  canvasMeta: CollaborationMeta | null;
  /** The signed-in user, as the server knows them. */
  userIdRef: React.RefObject<string | null>;
  onSynchronizationError: (error: Error | null) => void;
  createOrUpdateCursor: (userId: string, x: number, y: number, username: string) => void;
  hideCursor: (userId: string) => void;
  addParticipant: (userId: string, username: string, joinedAt: number, sessionId?: number) => void;
  clearParticipants: () => void;
  addChatMessage: (message: ChatLine) => void;
  handleResetRequest: () => void;
  /**
   * Whether this client could upload a checkpoint right now: caught up with
   * canonical history and holding a settled canvas. Answering a reset query
   * when this is false is how a room ends up waiting out the whole upload
   * window for a checkpoint that was never coming.
   */
  canUploadCheckpoint: () => boolean;
  onReconnectCanvas: (reconnecting: boolean, resumeSequence: number | null) => Promise<void> | void;
  onCanvasMessage: (message: DecodedMessage, raw: Uint8Array, sequence?: number) => Promise<void>;
  /** A sequenced frame this client cannot read; see `ReceiverHandlers`. */
  onUnreadableSequence: (sequence: number) => Promise<void>;
  onWelcome: (sessionId: number) => void;
  onResetPoint: (
    baseSequence: number,
    sequence: number | undefined,
    snapshotCount: number,
  ) => Promise<void> | void;
  verifyCanonicalPosition: () => Promise<boolean>;
  /**
   * The last sequence actually on the canvas, which is what a reconnect
   * resumes from. `link.lastSeq` is the last one received, and it runs ahead
   * of this across any gap -- a gap being the very thing that closes the
   * socket.
   */
  appliedCanonicalPosition: () => number;
  canResumeCanonicalPosition: () => boolean;
  onSessionEnded: (postUrl: string) => void;
  onSessionExpired: () => void;
}

const OPENING_PROGRESS: SyncProgress = {
  phase: "joining", receivedSequence: 0, appliedSequence: 0, targetSequence: null,
};

/** The connection's readable state, with one identity for the page's life. */
export const useSessionLink = (): SessionLink =>
  useRef<SessionLink>({
    connection: "connecting",
    catchingUp: true,
    localId: null,
    lastSeq: 0,
    shouldConnect: false,
  }).current;

/**
 * Opens the room's socket, keeps it open, and closes it.
 *
 * What arrives on it is the receiver's business (`sessionReceiver.ts`);
 * this hook decides when to connect, when a close is worth retrying, and
 * what the page is told about the connection.
 */
export const useWebSocket = ({
  link,
  canvasMeta,
  userIdRef,
  onSynchronizationError,
  createOrUpdateCursor,
  hideCursor,
  addParticipant,
  clearParticipants,
  addChatMessage,
  handleResetRequest,
  canUploadCheckpoint,
  onReconnectCanvas,
  onCanvasMessage,
  onUnreadableSequence,
  onWelcome,
  onResetPoint,
  verifyCanonicalPosition,
  appliedCanonicalPosition,
  canResumeCanonicalPosition,
  onSessionEnded,
  onSessionExpired,
}: WebSocketHookParams) => {
  const wsRef = useRef<WebSocket | null>(null);
  const [view, setView] = useState<SessionView>({
    connection: "connecting",
    catchingUp: true,
    progress: OPENING_PROGRESS,
  });
  /**
   * The one writer of what the page renders. The link is updated in the
   * same call, before React hears of it, so a message handled between this
   * and the render reads the value the render will show.
   */
  const publish = useCallback((change: Partial<SessionView>) => {
    if (change.connection !== undefined) link.connection = change.connection;
    if (change.catchingUp !== undefined) link.catchingUp = change.catchingUp;
    setView((current) => {
      const next = { ...current, ...change };
      return next.connection === current.connection &&
        next.catchingUp === current.catchingUp &&
        next.progress === current.progress
        ? current
        : next;
    });
  }, [link]);

  /**
   * Answers a reset query, if we are in a position to mean it.
   *
   * Non-owners hold back a moment first. Whoever answers first does the work,
   * and the session owner is the participant most likely to still be here when
   * it finishes -- this is the cheap version of Drawpile's ranked candidate
   * list, which weighs its volunteers before choosing between them. If the
   * owner is absent or unable, the beat passes and everyone else answers.
   */
  const answerResetQuery = useCallback(async () => {
    if (!canUploadCheckpoint()) {
      console.log("Checkpoint query declined: not caught up");
      return;
    }
    const isOwner = userIdRef.current === canvasMeta?.ownerId;
    if (!isOwner) {
      await new Promise((resolve) =>
        setTimeout(resolve, NON_OWNER_RESET_OFFER_DELAY_MS)
      );
      if (!canUploadCheckpoint()) return;
    }
    const ws = wsRef.current;
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(encodeResetOffer());
  }, [canUploadCheckpoint, canvasMeta?.ownerId, userIdRef]);

  /**
   * The page's callbacks, as of the latest render. The receiver reads them
   * when a frame arrives, so a re-render never rebuilds the receiver and
   * never touches the socket.
   */
  const handlersRef = useRef<ReceiverHandlers>(null!);
  handlersRef.current = {
    onSynchronizationError,
    createOrUpdateCursor,
    hideCursor,
    addParticipant,
    clearParticipants,
    addChatMessage,
    onResetQuery: () => { void answerResetQuery(); },
    onResetSelected: handleResetRequest,
    onReconnectCanvas,
    onCanvasMessage,
    onUnreadableSequence,
    onWelcome,
    onResetPoint,
    verifyCanonicalPosition,
    onSessionEnded,
    onSessionExpired,
  };
  const receiverRef = useRef<SessionReceiver | null>(null);
  if (receiverRef.current === null) {
    receiverRef.current = new SessionReceiver(link, publish, () => handlersRef.current);
  }
  const receiver = receiverRef.current;

  const isConnectingRef = useRef(false);
  // Reconnect bookkeeping. `hasConnectedRef` distinguishes the first connect
  // (blank canvas) from a reconnect (canvas still holds pre-disconnect pixels).
  const hasConnectedRef = useRef(false);
  const reconnectAttemptsRef = useRef(0);
  const reconnectTimerRef = useRef<number | null>(null);
  // Serializes async message processing so messages are always applied in
  // arrival order (the server's canonical order), even when handling involves
  // awaits like PNG decoding
  const processingChainRef = useRef<Promise<void>>(Promise.resolve());

  const getWebSocketUrl = useCallback(() => {
    // Check for explicitly set environment variable
    const envWsUrl = import.meta.env.VITE_WS_URL;
    if (envWsUrl) {
      return envWsUrl;
    }

    const isDevelopment = window.location.hostname === "localhost";
    const pathSegments = window.location.pathname.split("/");
    const sessionId = pathSegments[2]; // /collaborate/:sessionId
    const url = new URL(
      isDevelopment
        ? `ws://localhost:3000/collaborate/${sessionId}/ws`
        : `wss://${window.location.host}/collaborate/${sessionId}/ws`,
    );
    // The replay as batches, for a server that has them; one that does not
    // ignores the ask and sends a frame per message.
    url.searchParams.set("replay", "batch");
    // Resume only what the painter can carry on from: a settled fork at the
    // applied position. Anything else is a full replay.
    const resume = receiver.resumeFrom(
      hasConnectedRef.current && canResumeCanonicalPosition() ? appliedCanonicalPosition() : null,
    );
    if (resume) {
      url.searchParams.set("history_id", resume.historyId);
      url.searchParams.set("after_seq", String(resume.afterSeq));
    }
    return url.toString();
  }, [appliedCanonicalPosition, canResumeCanonicalPosition, receiver]);

  // Set after connectWebSocket is defined; lets the close handler retry
  // without depending on the callback identity.
  const connectRef = useRef<() => void>(() => {});

  const clearReconnectTimer = useCallback(() => {
    if (reconnectTimerRef.current !== null) {
      clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
  }, []);

  const scheduleReconnect = useCallback(() => {
    clearReconnectTimer();

    if (reconnectAttemptsRef.current >= RECONNECT_MAX_ATTEMPTS) {
      console.warn(
        `Giving up after ${RECONNECT_MAX_ATTEMPTS} reconnect attempts`
      );
      publish({ connection: "disconnected" });
      return;
    }

    const attempt = reconnectAttemptsRef.current++;
    const delay =
      Math.min(RECONNECT_MAX_MS, RECONNECT_BASE_MS * 2 ** attempt) +
      Math.random() * RECONNECT_JITTER_MS;

    // Stay in "connecting" so the UI shows the spinner rather than the
    // manual-reconnect modal while retries are still in flight.
    publish({ connection: "connecting" });
    console.log(
      `Reconnecting in ${Math.round(delay)}ms (attempt ${attempt + 1}/${RECONNECT_MAX_ATTEMPTS})`
    );
    reconnectTimerRef.current = window.setTimeout(() => {
      reconnectTimerRef.current = null;
      connectRef.current();
    }, delay);
  }, [clearReconnectTimer, publish]);

  const connectWebSocket = useCallback(async () => {
    // Only connect if we should be connecting
    if (!link.shouldConnect && wsRef.current) {
      return;
    }

    // Prevent multiple simultaneous connection attempts
    if (isConnectingRef.current) {
      return;
    }

    // If already connected, don't reconnect
    if (wsRef.current && wsRef.current.readyState === WebSocket.OPEN) {
      return;
    }

    // Set connecting flag
    isConnectingRef.current = true;
    clearReconnectTimer();

    // Clean up any existing connection. Detach its handlers first so closing
    // it here is not mistaken for a dropped connection worth retrying.
    if (wsRef.current) {
      wsRef.current.onclose = null;
      wsRef.current.onerror = null;
      wsRef.current.onmessage = null;
      wsRef.current.close();
      wsRef.current = null;
    }

    publish({ connection: "connecting" });

    // Check if we have user ID and canvas meta - don't proceed if not initialized
    if (!userIdRef.current || !canvasMeta) {
      console.error(
        "App not properly initialized - missing user ID or canvas meta"
      );
      publish({ connection: "disconnected" });
      isConnectingRef.current = false;
      return;
    }

    // The old socket's messages may still be on their way onto the canvas.
    // The resume position has to be taken after the last of them lands:
    // taken before, the server would send them again on top of themselves.
    await processingChainRef.current;

    try {
      const wsUrl = getWebSocketUrl();
      console.log("Creating WebSocket connection to:", wsUrl);
      const ws = new WebSocket(wsUrl);
      // Frames arrive as buffers, not Blobs. Every message this room produces
      // is binary, and the default would make each one a Blob that has to be
      // read back asynchronously -- on `processingChainRef`, which is strictly
      // serial, so that read sits on the critical path of every message from
      // everybody. Nothing here ever wanted a Blob.
      ws.binaryType = "arraybuffer";
      wsRef.current = ws;
    } catch (error) {
      console.error("Failed to create WebSocket:", error);
      publish({ connection: "disconnected" });
      isConnectingRef.current = false;
      return;
    }

    const ws = wsRef.current!;

    ws.onopen = async () => {
      console.log("WebSocket connected successfully:", ws.url);
      publish({ connection: "connected" });
      isConnectingRef.current = false;
      reconnectAttemptsRef.current = 0;

      const reconnecting = hasConnectedRef.current;
      hasConnectedRef.current = true;
      await receiver.opened(reconnecting);

      // Send initial join message to establish user presence
      try {
        ws.send(encodeJoin(userIdRef.current!, Date.now()));
      } catch (error) {
        console.error("Failed to send join message:", error);
      }
    };

    ws.onmessage = (event) => {
      // Chain message handling so messages are applied strictly in arrival
      // order even when processing involves awaits (e.g. PNG decoding)
      processingChainRef.current = processingChainRef.current
        .then(() => receiver.receive(ws, event.data))
        .catch((error) => {
          console.error("Failed to process WebSocket message:", error);
          receiver.fail(error);
          if (ws.readyState === WebSocket.OPEN) {
            ws.close(4000, "synchronization processing failed");
          }
        });
    };

    ws.onerror = (event) => {
      console.error("WebSocket error details:", {
        readyState: ws.readyState,
        url: ws.url,
        event: event,
      });
      isConnectingRef.current = false;
      // A close event always follows, which is where the retry is decided
    };

    ws.onclose = (event) => {
      console.log("WebSocket closed:", {
        code: event.code,
        reason: event.reason,
        wasClean: event.wasClean,
      });
      isConnectingRef.current = false;

      // Leaving the session (unmount, or the session ended) - stay closed
      if (!link.shouldConnect) {
        publish({ connection: "disconnected" });
        return;
      }

      // The server refused the join: the session is over or full, and no
      // amount of retrying will change that
      if (event.code === WS_CLOSE_POLICY) {
        console.warn("Server refused the session join:", event.reason);
        publish({ connection: "disconnected" });
        return;
      }

      // Anything else - a redeploy, a flaky network - is worth retrying, and
      // the modal's Reconnect button remains as the manual fallback once the
      // attempts run out
      scheduleReconnect();
    };
  }, [
    getWebSocketUrl,
    canvasMeta,
    link,
    publish,
    receiver,
    userIdRef,
    clearReconnectTimer,
    scheduleReconnect,
  ]);

  useEffect(() => {
    connectRef.current = () => {
      void connectWebSocket();
    };
  }, [connectWebSocket]);

  /**
   * Opens the connection, with an identity that never changes.
   *
   * `connectWebSocket` is rebuilt whenever any of the callbacks it closes
   * over is. An effect that depended on it therefore re-ran on those renders
   * and called it again, and every path that decides *not* to reconnect --
   * the backoff timer, a 1008 refusal from a session that is over -- ends by
   * setting connection state, which is itself a render. So the decision not
   * to reconnect scheduled the next reconnect: the timer was cleared and a
   * fresh socket opened at the speed of a round trip, and a refused join was
   * retried forever. Callers get this instead, so opening the socket is
   * something only a real event can ask for.
   */
  const connect = useCallback(() => {
    link.shouldConnect = true;
    connectRef.current();
  }, [link]);

  // Cleanup WebSocket on unmount
  useEffect(() => {
    return () => {
      if (reconnectTimerRef.current !== null) {
        clearTimeout(reconnectTimerRef.current);
        reconnectTimerRef.current = null;
      }
      if (wsRef.current) {
        wsRef.current.onclose = null;
        wsRef.current.close();
        wsRef.current = null;
      }
    };
  }, []);

  /**
   * Asks for no connection from here on: the page is leaving, or the session
   * is over. A socket already open is closed; a close that follows is not
   * retried.
   */
  const disconnect = useCallback(() => {
    link.shouldConnect = false;
    wsRef.current?.close();
  }, [link]);

  return {
    wsRef,
    /** Opens the connection and keeps it, until `disconnect`. */
    connect,
    disconnect,
    /** What the page renders about the connection; see `SessionView`. */
    view,
  };
};
