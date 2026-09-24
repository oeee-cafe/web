import { useCallback, useRef, useEffect } from "react";
import {
  decodeMessage,
  encodeJoin,
  encodeResetOffer,
  unwrapSequenced,
  type DecodedMessage,
  isCanvasHistoryMessage,
} from "../binaryProtocol";
import { type CollaborationMeta } from "../types";
import { acceptedResumeSequence } from "../synchronization";

export type ConnectionState = "disconnected" | "connecting" | "connected";

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
/**
 * How often the catch-up readout may publish a new position.
 *
 * It is a progress number on a modal, and publishing it re-renders the whole
 * session view. Ten times a second is faster than anybody reads.
 */
const PROGRESS_PUBLISH_MS = 100;

interface Participant {
  userId: string;
  username: string;
  joinedAt: number;
}

interface WebSocketHookParams {
  canvasMeta: CollaborationMeta | null;
  userIdRef: React.RefObject<string | null>;
  userLoginNameRef: React.RefObject<string>;
  localUserJoinTimeRef: React.RefObject<number>;
  participantsRef: React.RefObject<Map<string, Participant>>;
  localIdRef: React.RefObject<number | null>;
  lastSeqRef: React.RefObject<number>;
  shouldConnectRef: React.RefObject<boolean>;
  catchupTimeoutRef: React.RefObject<number | null>;
  processingMessageRef: React.RefObject<boolean>;
  isCatchingUpRef: React.RefObject<boolean>;
  setConnectionState: (state: ConnectionState) => void;
  setIsCatchingUp: (catching: boolean) => void;
  setSyncProgress: (progress: SyncProgress) => void;
  onSynchronizationError: (error: Error | null) => void;
  createOrUpdateCursor: (
    userId: string,
    x: number,
    y: number,
    username: string
  ) => void;
  hideCursor: (userId: string) => void;
  addParticipant: (
    userId: string,
    username: string,
    joinedAt: number,
    sessionId?: number,
  ) => void;
  clearParticipants: () => void;
  addChatMessage: (message: {
    id: string;
    type: "user" | "join" | "leave";
    userId: string;
    username: string;
    message: string;
    timestamp: number;
  }) => void;
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
  /**
   * A sequenced frame this client cannot read: a message type or tool code
   * newer than it is, or a frame truncated on its way into the history. It
   * still holds its place in canonical order, and the position has to be able
   * to step over it; see where it is called.
   */
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
   * resumes from. `lastSeqRef` is the last one received, and it runs ahead of
   * this across any gap -- a gap being the very thing that closes the socket.
   */
  appliedCanonicalPosition: () => number;
  canResumeCanonicalPosition: () => boolean;
  onSessionEnded: (postUrl: string) => void;
  onSessionExpired: () => void;
}

export const useWebSocket = ({
  canvasMeta,
  userIdRef,
  localUserJoinTimeRef,
  participantsRef,
  localIdRef,
  lastSeqRef,
  shouldConnectRef,
  catchupTimeoutRef,
  processingMessageRef,
  isCatchingUpRef,
  setConnectionState,
  setIsCatchingUp,
  setSyncProgress,
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
  const messageQueueRef = useRef<
    { message: DecodedMessage; raw: Uint8Array; seq?: number }[]
  >([]);
  const historyIdRef = useRef<string | null>(null);
  const caughtUpRef = useRef(false);
  const replayTargetRef = useRef<number | null>(null);
  const synchronizationFailedRef = useRef(false);
  const isConnectingRef = useRef(false);
  // Reconnect bookkeeping. `hasConnectedRef` distinguishes the first connect
  // (blank canvas) from a reconnect (canvas still holds pre-disconnect pixels).
  const hasConnectedRef = useRef(false);
  const reconnectPendingRef = useRef(false);
  const resumeRequestedRef = useRef(false);
  const reconnectAttemptsRef = useRef(0);
  const reconnectTimerRef = useRef<number | null>(null);
  // Serializes async message processing so messages are always applied in
  // arrival order (the server's canonical order), even when handling involves
  // awaits like PNG decoding
  const processingChainRef = useRef<Promise<void>>(Promise.resolve());
  // 1-byte session id -> display name (from LAYERS), for remote cursors
  const idNamesRef = useRef<Map<number, string>>(new Map());
  // uuid -> 1-byte session id, for presence events keyed by uuid
  const uuidToIdRef = useRef<Map<string, number>>(new Map());

  // Keep handleResetRequest ref to avoid dependency issues
  const handleResetRequestRef = useRef(handleResetRequest);
  useEffect(() => {
    handleResetRequestRef.current = handleResetRequest;
  }, [handleResetRequest]);

  const canUploadCheckpointRef = useRef(canUploadCheckpoint);
  useEffect(() => {
    canUploadCheckpointRef.current = canUploadCheckpoint;
  }, [canUploadCheckpoint]);

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
    if (!canUploadCheckpointRef.current()) {
      console.log("Checkpoint query declined: not caught up");
      return;
    }
    const isOwner = userIdRef.current === canvasMeta?.ownerId;
    if (!isOwner) {
      await new Promise((resolve) =>
        setTimeout(resolve, NON_OWNER_RESET_OFFER_DELAY_MS)
      );
      if (!canUploadCheckpointRef.current()) return;
    }
    const ws = wsRef.current;
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(encodeResetOffer());
  }, [canvasMeta?.ownerId, userIdRef]);

  const answerResetQueryRef = useRef(answerResetQuery);
  useEffect(() => {
    answerResetQueryRef.current = answerResetQuery;
  }, [answerResetQuery]);

  // Function to get WebSocket URL dynamically
  const getWebSocketUrl = useCallback(() => {
    // Check for explicitly set environment variable
    const envWsUrl = import.meta.env.VITE_WS_URL;
    if (envWsUrl) {
      return envWsUrl;
    }

    // Detect if we're in development
    const isDevelopment = window.location.hostname === "localhost";
    const pathSegments = window.location.pathname.split("/");
    const sessionId = pathSegments[2]; // /collaborate/:sessionId

    const appendResumePosition = (url: URL) => {
      resumeRequestedRef.current = false;
      if (
        hasConnectedRef.current &&
        historyIdRef.current !== null &&
        canResumeCanonicalPosition()
      ) {
        // Resume from what was applied, not from what was received. A socket
        // closed over a sequence gap has received past the hole, and asking
        // to continue after the last one received would have the server skip
        // the missing operations for good -- then clear everything held
        // beyond the hole, and call the canvas caught up. The painter's
        // canonical history ends here too, which is what lets a settled fork
        // carry on from it. Nothing has arrived on the new socket yet, so the
        // received position starts over from the same place.
        const applied = appliedCanonicalPosition();
        lastSeqRef.current = applied;
        url.searchParams.set("history_id", historyIdRef.current);
        url.searchParams.set("after_seq", String(applied));
        resumeRequestedRef.current = true;
      }
      return url.toString();
    };

    if (isDevelopment) {
      const url = new URL(`ws://localhost:3000/collaborate/${sessionId}/ws`);
      return appendResumePosition(url);
    }
    const url = new URL(`wss://${window.location.host}/collaborate/${sessionId}/ws`);
    return appendResumePosition(url);
  }, [appliedCanonicalPosition, canResumeCanonicalPosition, lastSeqRef]);

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
      setConnectionState("disconnected");
      return;
    }

    const attempt = reconnectAttemptsRef.current++;
    const delay =
      Math.min(RECONNECT_MAX_MS, RECONNECT_BASE_MS * 2 ** attempt) +
      Math.random() * RECONNECT_JITTER_MS;

    // Stay in "connecting" so the UI shows the spinner rather than the
    // manual-reconnect modal while retries are still in flight.
    setConnectionState("connecting");
    console.log(
      `Reconnecting in ${Math.round(delay)}ms (attempt ${attempt + 1}/${RECONNECT_MAX_ATTEMPTS})`
    );
    reconnectTimerRef.current = window.setTimeout(() => {
      reconnectTimerRef.current = null;
      connectRef.current();
    }, delay);
  }, [clearReconnectTimer, setConnectionState]);

  const connectWebSocket = useCallback(async () => {
    // Only connect if we should be connecting
    if (!shouldConnectRef.current && wsRef.current) {
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

    setConnectionState("connecting");

    // Check if we have user ID and canvas meta - don't proceed if not initialized
    if (!userIdRef.current || !canvasMeta) {
      console.error(
        "App not properly initialized - missing user ID or canvas meta"
      );
      setConnectionState("disconnected");
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
      setConnectionState("disconnected");
      isConnectingRef.current = false;
      return;
    }

    const ws = wsRef.current!;

    ws.onopen = async () => {
      console.log("WebSocket connected successfully:", ws.url);
      setConnectionState("connected");
      isConnectingRef.current = false;
      reconnectAttemptsRef.current = 0;

      // On reconnect the URL declares our last verified canonical position.
      // Keep the visible canvas until the first history identity tells us
      // whether the server accepted that position or fell back to full replay.
      const reconnecting = hasConnectedRef.current;
      reconnectPendingRef.current = reconnecting;
      if (!reconnecting) {
        await onReconnectCanvas(false, null);
        lastSeqRef.current = 0;
        historyIdRef.current = null;
      }
      hasConnectedRef.current = true;
      caughtUpRef.current = false;
      replayTargetRef.current = null;
      synchronizationFailedRef.current = false;
      onSynchronizationError(null);
      localIdRef.current = null; // reassigned by WELCOME

      // Send initial join message to establish user presence
      try {
        const binaryMessage = encodeJoin(userIdRef.current!, Date.now());
        ws.send(binaryMessage);
      } catch (error) {
        console.error("Failed to send join message:", error);
      }

      // Start catching up phase - drawing will be disabled
      setIsCatchingUp(true);
      setSyncProgress({
        phase: "joining",
        receivedSequence: 0,
        appliedSequence: 0,
        targetSequence: null,
      });

      // The server ends this phase explicitly with CAUGHT_UP. A timer cannot
      // distinguish an empty history from a delayed or truncated replay.
      if (catchupTimeoutRef.current) clearTimeout(catchupTimeoutRef.current);
      catchupTimeoutRef.current = null;

      // Set join timestamp after a short delay to let stored messages arrive first
      setTimeout(() => {
        if (localUserJoinTimeRef.current === 0) {
          // Only set if not already set
          localUserJoinTimeRef.current = Date.now();
        }
      }, 100); // 100ms should be enough for stored messages
    };

    const processIncomingData = async (data: ArrayBuffer | Blob) => {
      if (synchronizationFailedRef.current) return;
      // Clear any existing catch-up timeout since we now end catch-up when queue is empty
      if (catchupTimeoutRef.current) {
        clearTimeout(catchupTimeoutRef.current);
        catchupTimeoutRef.current = null;
      }

      let arrayBuffer =
        data instanceof ArrayBuffer ? data : await data.arrayBuffer();

      // History messages arrive wrapped in a SEQUENCED envelope carrying their
      // canonical position; the position is recorded only after the message
      // has been fully applied so lastSeq always describes the canvas state
      const sequenced = unwrapSequenced(arrayBuffer);
      if (sequenced) {
        if (reconnectPendingRef.current) {
          const resumeSequence = acceptedResumeSequence(
            resumeRequestedRef.current,
            { historyId: historyIdRef.current, sequence: lastSeqRef.current },
            sequenced.historyId,
            sequenced.seq,
            "entry",
          );
          await onReconnectCanvas(true, resumeSequence);
          reconnectPendingRef.current = false;
          if (resumeSequence === null) lastSeqRef.current = 0;
          historyIdRef.current = sequenced.historyId;
        }
        // No progress publish here. Every frame goes straight from this point
        // into the queue below and is applied before the next one is read, so
        // a "received" position was never visibly ahead of the applied one --
        // and publishing it per frame re-rendered the whole session view once
        // per replayed message, which is what the throttle in
        // `processMessageQueue` exists to prevent.
        if (historyIdRef.current === null) {
          historyIdRef.current = sequenced.historyId;
        } else if (historyIdRef.current !== sequenced.historyId) {
          console.error("Canonical history identity changed; reconnecting");
          setIsCatchingUp(true);
          ws.close(4000, "canonical history changed");
          return;
        }
        arrayBuffer = sequenced.payload;
      }

      const decoded = decodeMessage(arrayBuffer);
      // Inside a sequenced envelope only two things belong: a canvas operation,
      // and the reset point the server itself sequences. The server stores
      // whatever type byte a client sends it, so a WELCOME or an END_SESSION
      // can arrive here too -- and acted on, it would renumber or end the
      // session for everybody who reads the history. Such a frame still holds
      // its place, so it is stepped over exactly like one that will not decode.
      const message =
        decoded && sequenced &&
        !isCanvasHistoryMessage(decoded) && decoded.type !== "resetPoint"
          ? null
          : decoded;
      if (!message) {
        if (!sequenced) return;
        // Every client of this build rejects the same frame the same way, and
        // asking for it again would only bring it back: resuming from before
        // it is a reconnect loop. So it is stepped over as an operation that
        // does nothing, which is what the decoder's own policy already is --
        // an operation applied wrongly is worse than one not applied at all.
        console.warn(`Stepping over unreadable canonical message ${sequenced.seq}`);
        await onUnreadableSequence(sequenced.seq);
        lastSeqRef.current = Math.max(lastSeqRef.current, sequenced.seq);
        if (!isCatchingUpRef.current && !(await verifyCanonicalPosition())) {
          setIsCatchingUp(true);
          ws.close(4000, "canonical sequence gap");
        }
        return;
      }
      const raw = new Uint8Array(arrayBuffer);

      if (isCatchingUpRef.current) {
        // Pointer metadata describes the present moment, not replay state.
        if (message.type === "movePointer" || message.type === "pointerup") return;
        // During catch-up, queue messages for sequential processing
        messageQueueRef.current.push({ message, raw, seq: sequenced?.seq });
        // Process queue immediately if not already processing
        await processMessageQueue();
      } else {
        await handleBinaryMessage(message, raw, sequenced?.seq);

        if (sequenced) {
          lastSeqRef.current = Math.max(lastSeqRef.current, sequenced.seq);
          if (!(await verifyCanonicalPosition())) {
            setIsCatchingUp(true);
            ws.close(4000, "canonical sequence gap");
          }
        }
      }
    };

    ws.onmessage = (event) => {
      // Chain message handling so messages are applied strictly in arrival
      // order even when processing involves awaits (e.g. PNG decoding)
      processingChainRef.current = processingChainRef.current
        .then(() => processIncomingData(event.data))
        .catch((error) => {
          console.error("Failed to process WebSocket message:", error);
          const syncError = error instanceof Error ? error : new Error(String(error));
          synchronizationFailedRef.current = true;
          onSynchronizationError(syncError);
          setIsCatchingUp(true);
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
      if (!shouldConnectRef.current) {
        setConnectionState("disconnected");
        return;
      }

      // The server refused the join: the session is over or full, and no
      // amount of retrying will change that
      if (event.code === WS_CLOSE_POLICY) {
        console.warn("Server refused the session join:", event.reason);
        setConnectionState("disconnected");
        return;
      }

      // Anything else - a redeploy, a flaky network - is worth retrying, and
      // the modal's Reconnect button remains as the manual fallback once the
      // attempts run out
      scheduleReconnect();
    };

    // Process all queued messages immediately during catch-up
    const processMessageQueue = async () => {
      if (
        processingMessageRef.current ||
        messageQueueRef.current.length === 0
      ) {
        return;
      }

      processingMessageRef.current = true;
      const totalMessages = messageQueueRef.current.length;
      let publishedAt = 0;
      let lastAppliedSeq = lastSeqRef.current;

      // Process all messages immediately without artificial delays
      while (messageQueueRef.current.length > 0) {
        const { message, raw, seq } = messageQueueRef.current.shift()!;

        await handleBinaryMessage(message, raw, seq);

        if (seq !== undefined) {
          lastSeqRef.current = Math.max(lastSeqRef.current, seq);
          lastAppliedSeq = seq;
          // A number on a progress readout, published at the rate a history
          // replays. This is a state update on the session view -- the header,
          // the chat, the modal -- and a join replaying five hundred messages
          // was five hundred renders of all of it, for a counter nobody can
          // read faster than it moves.
          const now = performance.now();
          if (now - publishedAt >= PROGRESS_PUBLISH_MS) {
            publishedAt = now;
            setSyncProgress({
              phase: "applying",
              receivedSequence: lastSeqRef.current,
              appliedSequence: seq,
              targetSequence: replayTargetRef.current,
            });
          }
        }
      }

      // The last one always lands, whatever the throttle did with it: this is
      // the number the readout finishes on.
      setSyncProgress({
        phase: "applying",
        receivedSequence: lastSeqRef.current,
        appliedSequence: lastAppliedSeq,
        targetSequence: replayTargetRef.current,
      });

      processingMessageRef.current = false;
      console.log(`✅ Processed ${totalMessages} messages from catch-up queue`);

      // Queue exhaustion is not enough: a missing canonical sequence would
      // otherwise enable editing on an incomplete canvas.
      if (caughtUpRef.current && await verifyCanonicalPosition()) {
        setIsCatchingUp(false);
        setSyncProgress({
          phase: "ready",
          receivedSequence: lastSeqRef.current,
          appliedSequence: lastSeqRef.current,
          targetSequence: lastSeqRef.current,
        });
      } else if (caughtUpRef.current) {
        console.error("Canonical sequence gap after catch-up; reconnecting");
        ws.close(4000, "canonical sequence gap");
      }
    };

    // Helper function to handle decoded binary messages (moved inside connectWebSocket)
    const handleBinaryMessage = async (
      message: DecodedMessage,
      raw?: Uint8Array,
      seq?: number
    ) => {
      // Handle different message types. Errors deliberately escape to the
      // processing chain: continuing after a failed canvas operation would
      // make later sequence numbers appear healthy on a corrupt canvas.
      switch (message.type) {
          // All canvas-affecting messages fold into the shared canonical
          // canvas history, which owns conflict resolution (local fork
          // reconciliation) and collaborative undo
          case "stroke":
          case "fill":
          case "region":
          case "line":
          case "bezier":
          case "eraseAll":
          case "text":
          case "putImage":
          case "snapshot":
          case "undoPoint":
          case "undo": {
            // The list above is asserted against the vocabulary in
            // binaryProtocol.test.ts, so a kind cannot be added to one and
            // forgotten in the other.
            if (raw) await onCanvasMessage(message, raw, seq);

            // Show remote users' cursors at their latest drawing position
            if (
              message.userId !== localIdRef.current &&
              (message.type === "stroke" || message.type === "fill")
            ) {
              const username =
                idNamesRef.current.get(message.userId) || `#${message.userId}`;
              const point =
                message.type === "stroke"
                  ? message.points[message.points.length - 1]
                  : { x: message.x, y: message.y };
              if (point) {
                createOrUpdateCursor(
                  String(message.userId),
                  point.x,
                  point.y,
                  username
                );
              }
            }
            break;
          }

          case "welcome": {
            console.log("Assigned session user id:", message.sessionId);
            localIdRef.current = message.sessionId;
            onWelcome(message.sessionId);
            break;
          }

          case "resetPoint": {
            await onResetPoint(message.baseSeq, seq, message.snapshotCount);
            break;
          }

          case "replayStart": {
            replayTargetRef.current = message.lastSeq;
            if (reconnectPendingRef.current) {
              const resumeSequence = resumeRequestedRef.current
                && historyIdRef.current === message.historyId
                && lastSeqRef.current === message.afterSeq
                ? lastSeqRef.current
                : null;
              await onReconnectCanvas(true, resumeSequence);
              reconnectPendingRef.current = false;
              if (resumeSequence === null) lastSeqRef.current = 0;
              historyIdRef.current = message.historyId;
            }
            setSyncProgress({
              phase: "receiving",
              receivedSequence: message.afterSeq,
              appliedSequence: message.afterSeq,
              targetSequence: message.lastSeq,
            });
            break;
          }

          case "caughtUp": {
            if (reconnectPendingRef.current) {
              const resumeSequence = acceptedResumeSequence(
                resumeRequestedRef.current,
                { historyId: historyIdRef.current, sequence: lastSeqRef.current },
                message.historyId,
                message.lastSeq,
                "caughtUp",
              );
              await onReconnectCanvas(true, resumeSequence);
              reconnectPendingRef.current = false;
              if (resumeSequence === null) lastSeqRef.current = 0;
              historyIdRef.current = message.historyId;
            }
            if (historyIdRef.current === null) {
              historyIdRef.current = message.historyId;
            }
            if (historyIdRef.current !== message.historyId) {
              ws.close(4000, "caught-up history mismatch");
              break;
            }
            lastSeqRef.current = message.lastSeq;
            replayTargetRef.current = message.lastSeq;
            caughtUpRef.current = true;
            setSyncProgress({
              phase: "applying",
              receivedSequence: lastSeqRef.current,
              appliedSequence: Math.min(lastSeqRef.current, seq ?? lastSeqRef.current),
              targetSequence: message.lastSeq,
            });
            break;
          }

          case "pointerup": {
            // Hide cursor for remote users when they stop drawing
            if (message.userId !== localIdRef.current) {
              hideCursor(String(message.userId));
            }
            break;
          }

          case "movePointer": {
            // Not while catching up. A pointer position is where somebody is
            // right now, and during a replay it is neither: the positions
            // arriving are from whenever the history was made, and drawing
            // them puts other people's cursors on a canvas that has not
            // finished being rebuilt.
            if (isCatchingUpRef.current) break;
            if (message.userId !== localIdRef.current) {
              const username =
                idNamesRef.current.get(message.userId) || `#${message.userId}`;
              createOrUpdateCursor(
                String(message.userId), message.x, message.y, username,
              );
            }
            break;
          }

          case "join": {
            // Don't add participant here - wait for LAYERS message
            // This ensures consistent participant ordering from server

            // Add join notification to chat
            addChatMessage({
              id: `${message.userId}-${message.timestamp}-join`,
              type: "join" as const,
              userId: message.userId,
              username: message.username,
              message: `${message.username} joined`,
              timestamp: message.timestamp,
            });
            break;
          }

          case "leave": {
            // Add leave notification to chat
            addChatMessage({
              id: `${message.userId}-${message.timestamp}-leave`,
              type: "leave" as const,
              userId: message.userId,
              username: message.username,
              message: `${message.username} left the session`,
              timestamp: message.timestamp,
            });

            // Hide cursor for the user (but keep participant in the list)
            const sessionId = uuidToIdRef.current.get(message.userId);
            if (sessionId !== undefined) {
              hideCursor(String(sessionId));
            }
            break;
          }

          case "chat": {
            // Add chat message to the chat component via the callback
            addChatMessage({
              id: `${message.userId}-${message.timestamp}`,
              type: "user" as const,
              userId: message.userId,
              username: message.username,
              message: message.message,
              timestamp: message.timestamp,
            });
            break;
          }

          case "layers": {
            console.log("Layers message received:", {
              participantCount: message.participants.length,
            });

            // Clear existing participants to avoid inconsistencies
            // This ensures all clients have identical participant ordering from server
            participantsRef.current?.clear();
            clearParticipants();

            // Sort participants by join timestamp (already sorted on the
            // server, but we verify here)
            const sortedParticipants = message.participants.sort(
              (a, b) => a.joinTimestamp - b.joinTimestamp
            );

            for (const participant of sortedParticipants) {
              addParticipant(
                participant.userId,
                participant.username,
                participant.joinTimestamp,
                participant.sessionId > 0 ? participant.sessionId : undefined
              );
              if (participant.sessionId > 0) {
                idNamesRef.current.set(
                  participant.sessionId,
                  participant.username
                );
                uuidToIdRef.current.set(
                  participant.userId,
                  participant.sessionId
                );
              }
            }
            break;
          }

          case "resetRequest": {
            console.log("Session reset request received:", {
              timestamp: message.timestamp,
              phase: message.phase,
            });

            if (message.phase === "query") {
              // The room is asking who can upload a checkpoint. Answering is
              // one byte; the work only starts if we win.
              void answerResetQueryRef.current();
            } else {
              // We answered first, so it is ours to do.
              handleResetRequestRef.current();
            }
            break;
          }

          case "endSession": {
            console.log("Session ended:", {
              userId: message.userId.substring(0, 8),
              postUrl: message.postUrl,
            });

            if (message.postUrl) {
              shouldConnectRef.current = false;
              onSessionEnded(message.postUrl);
            }
            break;
          }

          case "sessionExpired": {
            shouldConnectRef.current = false;
            setConnectionState("disconnected");
            onSessionExpired();
            break;
          }

          default: {
            console.log("Unknown message type:", message);
            break;
          }
      }
    };
  }, [
    getWebSocketUrl,
    canvasMeta,
    localIdRef,
    lastSeqRef,
    setConnectionState,
    setIsCatchingUp,
    setSyncProgress,
    onSynchronizationError,
    createOrUpdateCursor,
    hideCursor,
    addParticipant,
    clearParticipants,
    addChatMessage,
    catchupTimeoutRef,
    isCatchingUpRef,
    localUserJoinTimeRef,
    participantsRef,
    processingMessageRef,
    shouldConnectRef,
    userIdRef,
    clearReconnectTimer,
    scheduleReconnect,
    onReconnectCanvas,
    onCanvasMessage,
    onUnreadableSequence,
    onWelcome,
    onResetPoint,
    verifyCanonicalPosition,
    onSessionEnded,
    onSessionExpired,
  ]);

  useEffect(() => {
    connectRef.current = () => {
      void connectWebSocket();
    };
  }, [connectWebSocket]);

  /**
   * Opens the connection, with an identity that never changes.
   *
   * `connectWebSocket` is rebuilt whenever any of the many callbacks it closes
   * over is, which is to say on most renders of the session view. An effect
   * that depended on it therefore re-ran on most renders and called it again,
   * and every path that decides *not* to reconnect -- the backoff timer, a
   * 1008 refusal from a session that is over -- ends by setting connection
   * state, which is itself a render. So the decision not to reconnect
   * scheduled the next reconnect: the timer was cleared and a fresh socket
   * opened at the speed of a round trip, and a refused join was retried
   * forever. Callers get this instead, so opening the socket is something only
   * a real event can ask for.
   */
  const connect = useCallback(() => {
    connectRef.current();
  }, []);

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

  return {
    wsRef,
    connect,
    getWebSocketUrl,
  };
};
