import { useCallback, useEffect, useRef, useState } from "react";
import { Trans, useLingui } from "@lingui/react/macro";
import {
  anchorBesideCanvas,
  attachWindowDrag,
  attachWindowResize,
  mount,
  NEO_BUTTON,
  NEO_PANEL,
  NEO_PANEL_BUTTON,
  minimumTop,
  PANEL_MARGIN,
  NEO_RESIZE_GRIP,
  NEO_RESIZE_HANDLE,
  NEO_TITLEBAR_DOT,
  NEO_TITLEBAR_HANDLE,
  type CanonicalPainterOperation,
  type LocalPainterOperation,
  type PainterCheckpoint,
  type PainterCheckpointLayers,
  type PainterHandle,
} from "neo-cucumber";
import "./app.css";
import { Chat } from "./components/Chat";
import { SessionExpiredModal } from "./components/SessionExpiredModal";
import { SessionHeader } from "./components/SessionHeader";
import { AuthErrorModal } from "./components/modals/AuthErrorModal";
import { ConnectionStatusModal } from "./components/modals/ConnectionStatusModal";
import { InitializationErrorModal } from "./components/modals/InitializationErrorModal";
import { LoadingModal } from "./components/modals/LoadingModal";
import { RoomFullModal } from "./components/modals/RoomFullModal";
import { SaveConfirmationModal } from "./components/modals/SaveConfirmationModal";
import { SessionEndingModal } from "./components/modals/SessionEndingModal";
import {
  decodePainterOperation,
  encodeEndSession,
  encodeMovePointer,
  encodePainterOperation,
  encodePointerUp,
  encodeResetBegin,
  encodeSnapshot,
  type DecodedMessage,
} from "./binaryProtocol";
import { useWebSocket, type ConnectionState, type SyncProgress } from "./hooks/useWebSocket";
import { useRemoteCursors } from "./hooks/useRemoteCursors";
import { reportDiagnostics, type DiagnosticContext } from "./diagnostics";
import { refreshSessionPreview } from "./preview";
import type { CollaborationMeta, Participant } from "./types";

/** How long the owner waits for the server to confirm the end of the session
 * before going to the saved post anyway. */
const SAVE_CONFIRMATION_TIMEOUT_MS = 5000;

/**
 * How often this client offers to refresh the lobby's picture of this canvas.
 *
 * Shorter than the window the server paces refreshes with, so whoever ticks
 * first after one opens takes it; the lobby's own poll is what decides how
 * quickly a new preview then appears on a card. Every tick that finds nothing
 * new to show, or a window somebody else holds, costs one small request.
 */
const PREVIEW_ATTEMPT_INTERVAL_MS = 15000;

/**
 * The shortest gap between two cursor positions going out.
 *
 * Every one of these is sequenced by nobody but still fans out to the whole
 * room, where it costs a decode and a repaint of one small element. Thirty a
 * second reads as continuous motion; the display's refresh rate, which is what
 * a bare `requestAnimationFrame` gives, is up to four times that on a modern
 * screen and looks no different to anybody watching.
 */
const POINTER_BROADCAST_MS = 33;

/**
 * How long the canonical stream may hold the main thread before it has to let
 * go of it.
 *
 * There is one thread here, and applying somebody else's marks runs on the
 * same one that is meant to be following this user's pen. A burst -- three
 * people drawing, or the tail of a catch-up -- used to be applied to
 * exhaustion, because every `await` inside it is a microtask and microtasks
 * are not a yield: the queue drains completely before the browser is allowed
 * to deliver the next pointer event.
 *
 * Drawpile bounds the same work at about 0.2ms per batch, which it can afford
 * because its paint engine has a thread to itself and is emptied again on the
 * next tick. Ours has to share, so the budget is most of a frame rather than a
 * fraction of one, and what follows it is a real yield.
 */
const CANONICAL_BUDGET_MS = 6;

/**
 * Hands the thread back long enough for input to be delivered.
 *
 * `scheduler.yield` resumes at a priority above an ordinary task, so the drain
 * picks up again ahead of anything incidental; without it a message channel is
 * the cheapest macrotask that still lets the browser run pending input first.
 * A `setTimeout` is not a substitute -- nested timeouts are clamped to 4ms,
 * which would cost more than the work being interrupted.
 */
const yieldToInput = (): Promise<void> => {
  const { scheduler } = globalThis as unknown as {
    scheduler?: { yield?: () => Promise<void> };
  };
  if (typeof scheduler?.yield === "function") return scheduler.yield();
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    channel.port1.onmessage = () => {
      channel.port1.close();
      resolve();
    };
    channel.port2.postMessage(null);
  });
};

const getSessionId = (): string => {
  const id = window.location.pathname.split("/")[2];
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id ?? "")) {
    throw new Error("Invalid session ID in URL");
  }
  return id;
};

/** Two hex digits for every byte value, so the loop below never formats one. */
const HEX_OCTETS = Array.from({ length: 256 }, (_, value) =>
  value.toString(16).padStart(2, "0"),
);

/**
 * The bytes of a message, as the string used to recognise its echo.
 *
 * Runs on every operation this client sends and every canvas message it
 * receives. Built through `Array.from(...).join("")` it allocated an array and
 * two short-lived strings per byte -- some hundreds of allocations for one
 * stroke chunk, at the rate a busy room produces them. The output is the same
 * hex string; only the garbage is gone.
 */
const bytesId = (bytes: Uint8Array): string => {
  let id = "";
  for (let index = 0; index < bytes.length; index++) {
    id += HEX_OCTETS[bytes[index]];
  }
  return id;
};

const downloadBlob = (blob: Blob, name: string) => {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = name;
  anchor.click();
  URL.revokeObjectURL(url);
};

const blankLayer = (width: number, height: number): Promise<Blob> => {
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  return new Promise((resolve, reject) =>
    canvas.toBlob((blob) => blob ? resolve(blob) : reject(new Error("Could not create blank layer")), "image/png"),
  );
};

/** One line of the session chat, as the chat window and the socket both spell it. */
type ChatMessage = {
  id: string;
  type: "join" | "leave" | "user";
  userId: string;
  username: string;
  message: string;
  timestamp: number;
};

export default function App() {
  const { t } = useLingui();
  const [canvasMeta, setCanvasMeta] = useState<CollaborationMeta | null>(null);
  const [initializationError, setInitializationError] = useState<string | null>(null);
  const [participants, setParticipants] = useState<Map<string, Participant>>(new Map());
  const [connectionState, setConnectionState] = useState<ConnectionState>("connecting");
  const [isCatchingUp, setIsCatchingUp] = useState(true);
  const [authError, setAuthError] = useState(false);
  const [roomFullError, setRoomFullError] = useState<{ currentUserCount: number; maxUsers: number } | null>(null);
  const [isSaving, setIsSaving] = useState(false);
  const [sessionExpired, setSessionExpired] = useState(false);
  const [sessionEnding, setSessionEnding] = useState(false);
  const [saveConfirmation, setSaveConfirmation] = useState(false);
  const [syncProgress, setSyncProgress] = useState<SyncProgress>({
    phase: "joining", receivedSequence: 0, appliedSequence: 0, targetSequence: null,
  });
  const [synchronizationError, setSynchronizationError] = useState<string | null>(null);

  // The chat window opens where the toolbox's own windows do, and is held out
  // of the header for the same reason they are: it would cover the session
  // title, the share button and the way back to the lobby. Both numbers are
  // measured from the painter's area rather than written out here, so the two
  // kinds of window cannot drift apart.
  const [chatPosition, setChatPosition] = useState({
    x: PANEL_MARGIN,
    y: PANEL_MARGIN,
  });
  const [chatCeiling, setChatCeiling] = useState(0);
  /** The painter's controls and its opening zoom are both settled. */
  const [painterReady, setPainterReady] = useState(false);
  // Half the height it used to open at. It was tall enough to reach most of
  // the way down the drawing, and the layers window now shares that column.
  const [chatSize, setChatSize] = useState({ width: 224, height: 234 });
  const chatFrameRef = useRef<HTMLDivElement>(null);
  const chatHandleRef = useRef<HTMLDivElement>(null);
  const chatResizeRef = useRef<HTMLDivElement>(null);

  const painterElementRef = useRef<HTMLDivElement>(null);
  const painterRef = useRef<PainterHandle | null>(null);
  const saveButtonRef = useRef<HTMLButtonElement>(null);
  const saveProxyRef = useRef<HTMLButtonElement | null>(null);
  const userIdRef = useRef("");
  /**
   * The locale the server resolved for this user.
   *
   * Handed to the painter when it mounts. Lingui's `i18n` is one shared
   * instance, so a painter mounted without a locale does not merely leave its
   * own toolbox in English -- it activates English over whatever this page had
   * already chosen, taking the session header and the chat with it.
   */
  const localeRef = useRef<string | undefined>(undefined);
  /**
   * Joining happens once.
   *
   * Everything downstream hangs off the metadata this fetches -- the painter is
   * mounted from it, and mounting the painter activates a locale, which is
   * enough to make a hook that merely reads a translation look changed. Running
   * this twice does not just repeat two requests: it hands back a new metadata
   * object, which remounts the painter, which activates again.
   */
  const joinedRef = useRef(false);
  const userLoginNameRef = useRef("");
  const localUserJoinTimeRef = useRef(0);
  const localIdRef = useRef<number | null>(null);
  const lastSeqRef = useRef(0);
  const participantsRef = useRef(participants);
  const shouldConnectRef = useRef(false);
  const catchupTimeoutRef = useRef<number | null>(null);
  const processingMessageRef = useRef(false);
  const isCatchingUpRef = useRef(true);
  const pendingIdsRef = useRef(new Map<string, string[]>());
  const pointerFrameRef = useRef<number | null>(null);
  const pendingPointerRef = useRef<{ x: number; y: number } | null>(null);
  /** When the last cursor position went out, and where it was. */
  const pointerSentAtRef = useRef(0);
  const lastSentPointerRef = useRef<{ x: number; y: number } | null>(null);
  const expectedSequenceRef = useRef(1);
  const appliedSequenceRef = useRef(0);
  const canonicalOperationsRef = useRef(new Map<number, CanonicalPainterOperation>());
  /**
   * Snapshots of a pending checkpoint, by sequence then by participant.
   *
   * Every snapshot of one reset carries the same sequence, and a checkpoint
   * now covers a layer pair per participant, so the group is only whole once
   * RESET_POINT has said how many snapshots to expect and that many have
   * arrived. Applying a partial group would blank whoever was still in
   * flight.
   */
  const snapshotPairsRef = useRef(
    new Map<number, Map<string, Partial<Pick<PainterCheckpointLayers, "background" | "foreground">>>>(),
  );
  const snapshotCountsRef = useRef(new Map<number, number>());
  const canonicalDrainRef = useRef<Promise<void>>(Promise.resolve());
  const { createOrUpdateCursor, hideCursor, clearCursors } = useRemoteCursors(
    painterElementRef, localIdRef,
  );
  const chatAddMessageRef = useRef<((message: ChatMessage) => void) | null>(null);
  /**
   * Stable identities for the chat's three callbacks.
   *
   * Written inline they were new functions on every render of this component,
   * and the chat re-runs an effect on the one it is handed -- so a render
   * caused by anything at all, a progress counter most of all, re-ran it. They
   * close over nothing that changes.
   *
   * `addChatMessage` is handed to the WebSocket hook rather than to the chat,
   * where an unstable identity cost more than a re-run effect: it is one of
   * the dependencies `connectWebSocket` is built from, so an inline arrow here
   * rebuilt the socket callback on every render and re-fired the effect that
   * opens the connection. See the connect effect below.
   */
  const holdChatAddMessage = useCallback(
    (add: NonNullable<typeof chatAddMessageRef.current>) => {
      chatAddMessageRef.current = add;
    },
    [],
  );
  const addChatMessage = useCallback(
    (message: ChatMessage) => chatAddMessageRef.current?.(message),
    [],
  );
  const noopChatMessage = useCallback(() => {}, []);

  useEffect(() => { participantsRef.current = participants; }, [participants]);
  /**
   * Name the layers for the painter's participant toolbox.
   *
   * The painter knows which actors have drawn -- their layers exist -- but the
   * roster is ours. The session ids the canonical stream uses are what its
   * layers are keyed by, so those are what it is told.
   */
  useEffect(() => {
    painterRef.current?.setParticipants(
      [...participants.values()]
        .filter((participant) => participant.sessionId !== undefined)
        .map((participant) => ({
          actorId: String(participant.sessionId),
          name: participant.username,
        })),
    );
  }, [participants]);
  useEffect(() => { isCatchingUpRef.current = isCatchingUp; }, [isCatchingUp]);

  const clearParticipants = useCallback(() => setParticipants(new Map()), []);
  const addParticipant = useCallback((
    userId: string, username: string, joinedAt: number, sessionId?: number,
  ) => {
    setParticipants((current) =>
      new Map(current).set(userId, { userId, username, joinedAt, sessionId }));
  }, []);

  const onLocalOperation = useCallback((entry: LocalPainterOperation) => {
    const ws = wsRef.current;
    const localId = localIdRef.current;
    if (!ws || ws.readyState !== WebSocket.OPEN || localId === null || isCatchingUpRef.current) return;
    const encoded = encodePainterOperation(localId, entry.operation);
    const wireId = bytesId(new Uint8Array(encoded));
    const queue = pendingIdsRef.current.get(wireId) ?? [];
    queue.push(entry.id);
    pendingIdsRef.current.set(wireId, queue);
    ws.send(encoded);
  // wsRef is created by the WebSocket hook below and is stable for its lifetime.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onLocalPointerUp = useCallback(() => {
    const ws = wsRef.current;
    const localId = localIdRef.current;
    if (!ws || ws.readyState !== WebSocket.OPEN || localId === null) return;
    ws.send(encodePointerUp(localId));
  // wsRef is created by the WebSocket hook below and is stable for its lifetime.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onLocalPointerMove = useCallback((position: { x: number; y: number } | null) => {
    if (!position) {
      pendingPointerRef.current = null;
      lastSentPointerRef.current = null;
      if (pointerFrameRef.current !== null) cancelAnimationFrame(pointerFrameRef.current);
      pointerFrameRef.current = null;
      onLocalPointerUp();
      return;
    }
    pendingPointerRef.current = position;
    if (pointerFrameRef.current !== null) return;
    const sendPending = () => {
      pointerFrameRef.current = null;
      const point = pendingPointerRef.current;
      const ws = wsRef.current;
      const localId = localIdRef.current;
      if (!point || ws?.readyState !== WebSocket.OPEN || localId === null) return;
      // A frame is the display's rate, not a useful rate for somebody else's
      // cursor: on a 120Hz screen this fired twice as often as on a 60Hz one,
      // and every one of those is a message the whole room decodes. This
      // handler also runs on plain hover, so an idle pointer resting over the
      // canvas was costing everybody else the same as one that was drawing.
      //
      // Too soon means wait for the next frame, not drop it: a flick that ends
      // inside the interval must still deliver where it ended, or the cursor
      // stops short of where its owner is.
      if (performance.now() - pointerSentAtRef.current < POINTER_BROADCAST_MS) {
        pointerFrameRef.current = requestAnimationFrame(sendPending);
        return;
      }
      // Canvas coordinates, so at a zoomed-out view several screen pixels of
      // travel land on the pixel already sent.
      const last = lastSentPointerRef.current;
      if (last && last.x === point.x && last.y === point.y) return;
      pointerSentAtRef.current = performance.now();
      lastSentPointerRef.current = { x: point.x, y: point.y };
      ws.send(encodeMovePointer(localId, point.x, point.y));
    };
    pointerFrameRef.current = requestAnimationFrame(sendPending);
  // wsRef is created by the WebSocket hook below and is stable for its lifetime.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [onLocalPointerUp]);

  useEffect(() => {
    const element = painterElementRef.current;
    if (!element || !canvasMeta || painterRef.current) return;
    const painter = mount(element, {
      width: canvasMeta.width,
      height: canvasMeta.height,
      // Read from /api/auth above, which resolves the user's own preference
      // before falling back to what the browser asked for.
      locale: localeRef.current,
      mode: { kind: "standard" },
      controls: { kind: "toolbox" },
      // A session saves a flattened image and never asks for a replay, and
      // `.pch` could not describe one anyway: it addresses two layers, and
      // every participant here has a pair.
      recordReplay: false,
      synchronization: {
        // Stands in only until WELCOME assigns the session id every canonical
        // message is keyed by; see `handleWelcome`.
        actorId: userIdRef.current,
        onOperation: onLocalOperation,
        onPointerMove: onLocalPointerMove,
        onPointerUp: onLocalPointerUp,
      },
    });
    painterRef.current = painter;
    void painter.ready.then(() => {
      painter.setInteractionEnabled(false);
      setPainterReady(true);
    });
    return () => {
      painter.unmount();
      if (painterRef.current === painter) painterRef.current = null;
    };
  }, [canvasMeta, onLocalOperation, onLocalPointerMove, onLocalPointerUp]);

  useEffect(() => () => {
    if (pointerFrameRef.current !== null) cancelAnimationFrame(pointerFrameRef.current);
  }, []);

  /**
   * Open the chat against the left edge of the drawing.
   *
   * The toolbox takes the right, so the chat takes the left, and both are
   * placed by the same function from the package -- on a wide display the edge
   * of the screen is nowhere near the canvas, and a chat window parked out
   * there is as far from the drawing as the toolbox used to be.
   *
   * It waits for the painter to be ready, which is when the opening zoom has
   * been applied: until then the canvas is the size it was declared at rather
   * than the size it is shown at.
   */
  useEffect(() => {
    if (!painterReady) return;
    const painterElement = painterElementRef.current;
    const area = painterElement?.getBoundingClientRect() ?? null;
    setChatCeiling(minimumTop(area));
    const canvas =
      painterElement
        ?.querySelector<HTMLElement>(".canvas-container")
        ?.getBoundingClientRect() ?? null;
    const opening = anchorBesideCanvas(area, canvas, chatSize.width, "left");
    setChatPosition(opening);
    // The layers window goes under the chat, in the same column. The painter
    // would otherwise put it beside its own toolboxes, where it knows nothing
    // about the chat and would open on top of it.
    painterRef.current?.setLayersOrigin({
      x: opening.x,
      y: opening.y + chatSize.height + PANEL_MARGIN,
    });
    // The opening position only: afterwards the window is where it was dragged.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [painterReady]);

  useEffect(() => {
    const frame = chatFrameRef.current;
    const handle = chatHandleRef.current;
    if (!frame || !handle) return;
    return attachWindowDrag(frame, handle, {
      minimumY: chatCeiling,
      onPosition: setChatPosition,
    });
  }, [chatCeiling]);

  useEffect(() => {
    const frame = chatFrameRef.current;
    const corner = chatResizeRef.current;
    if (!frame || !corner) return;
    return attachWindowResize(frame, corner, {
      minimum: { width: 180, height: 140 },
      onSize: setChatSize,
    });
  }, []);

  useEffect(() => {
    painterRef.current?.setInteractionEnabled(
      connectionState === "connected" && !isCatchingUp && !sessionEnding && !sessionExpired,
    );
  }, [connectionState, isCatchingUp, sessionEnding, sessionExpired]);

  const drainCanonical = useCallback(async () => {
    canonicalDrainRef.current = canonicalDrainRef.current.then(async () => {
      const painter = painterRef.current;
      if (!painter || !canvasMeta) return;
      let deadline = performance.now() + CANONICAL_BUDGET_MS;
      while (true) {
        const expected = expectedSequenceRef.current;
        const operation = canonicalOperationsRef.current.get(expected);
        if (operation) {
          canonicalOperationsRef.current.delete(expected);
          await painter.applyCanonicalOperation(operation);
          appliedSequenceRef.current = expected;
          expectedSequenceRef.current = expected + 1;
          // Not while catching up: the painter takes no input until the replay
          // is done, so there is nothing to be responsive to and yielding
          // would only make the wait longer.
          if (!isCatchingUpRef.current && performance.now() >= deadline) {
            await yieldToInput();
            deadline = performance.now() + CANONICAL_BUDGET_MS;
          }
          continue;
        }

        // A checkpoint is a legal jump over compacted history. Every
        // participant's pair must arrive before it replaces the canvas and
        // advances the sequence, which is what the announced count settles.
        const isWhole = (sequence: number, owners: Map<string, Partial<PainterCheckpointLayers>>) => {
          const expectedCount = snapshotCountsRef.current.get(sequence);
          if (expectedCount === undefined) return false;
          let held = 0;
          for (const pair of owners.values()) {
            held += (pair.background ? 1 : 0) + (pair.foreground ? 1 : 0);
          }
          return held >= expectedCount;
        };
        const checkpointSequence = [...snapshotPairsRef.current.entries()]
          .filter(([sequence, owners]) => sequence >= expected && isWhole(sequence, owners))
          .map(([sequence]) => sequence)
          .sort((a, b) => a - b)[0];
        if (checkpointSequence === undefined) break;
        const owners = snapshotPairsRef.current.get(checkpointSequence)!;
        await painter.applyCheckpoint({
          sequence: checkpointSequence,
          width: canvasMeta.width,
          height: canvasMeta.height,
          layers: [...owners]
            // Ascending session id is join order, and it never changes, so
            // every client composites the same stack.
            .sort(([a], [b]) => Number(a) - Number(b))
            .map(([actorId, pair]) => ({
              actorId,
              background: pair.background!,
              foreground: pair.foreground!,
            })),
        });
        snapshotPairsRef.current.delete(checkpointSequence);
        snapshotCountsRef.current.delete(checkpointSequence);
        for (const sequence of canonicalOperationsRef.current.keys()) {
          if (sequence <= checkpointSequence) canonicalOperationsRef.current.delete(sequence);
        }
        appliedSequenceRef.current = checkpointSequence;
        expectedSequenceRef.current = checkpointSequence + 1;
      }
    });
    await canonicalDrainRef.current;
  }, [canvasMeta]);

  const onCanvasMessage = useCallback(async (
    message: DecodedMessage, raw: Uint8Array, sequence?: number,
  ) => {
    if (sequence === undefined) {
      throw new Error("Received an unsequenced canvas-history message");
    }
    if (message.type === "snapshot") {
      const pngBytes = new Uint8Array(message.pngData).slice();
      const owners = snapshotPairsRef.current.get(sequence) ?? new Map();
      // Whose layer this is, not who sent it. One client uploads the whole
      // checkpoint on everyone's behalf, so `userId` is the same uploader for
      // every snapshot in it -- keying by that collapsed all of them onto one
      // participant, left the checkpoint one pair short of the count that says
      // it is whole, and so stopped it from ever being applied.
      const actorId = String(message.targetOwner);
      const pair = owners.get(actorId) ?? {};
      pair[message.layer] = new Blob([pngBytes.buffer as ArrayBuffer], { type: "image/png" });
      owners.set(actorId, pair);
      snapshotPairsRef.current.set(sequence, owners);
      await drainCanonical();
      return;
    }
    const operation = decodePainterOperation(message);
    if (!operation || !("userId" in message) || typeof message.userId !== "number") return;
    const wireId = bytesId(raw);
    const pendingIds = pendingIdsRef.current.get(wireId);
    const id = pendingIds?.shift() ?? wireId;
    if (pendingIds?.length === 0) pendingIdsRef.current.delete(wireId);
    const canonical: CanonicalPainterOperation = {
      id,
      actorId: String(message.userId),
      sequence,
      operation,
    };
    canonicalOperationsRef.current.set(sequence, canonical);
    await drainCanonical();
  }, [drainCanonical]);

  const onReconnectCanvas = useCallback(async (
    reconnecting: boolean, resumeSequence: number | null,
  ) => {
    clearCursors();
    pendingIdsRef.current.clear();
    canonicalOperationsRef.current.clear();
    snapshotPairsRef.current.clear();
    snapshotCountsRef.current.clear();
    if (reconnecting && resumeSequence !== null) {
      expectedSequenceRef.current = resumeSequence + 1;
      appliedSequenceRef.current = resumeSequence;
      return;
    }
    expectedSequenceRef.current = 1;
    appliedSequenceRef.current = 0;
    if (!reconnecting || !canvasMeta || !painterRef.current) return;
    const layer = await blankLayer(canvasMeta.width, canvasMeta.height);
    // No participant's layers survive a full replay, so the checkpoint names
    // only ourselves; applyCheckpoint blanks everyone it does not mention.
    const checkpoint: PainterCheckpoint = {
      sequence: 0, width: canvasMeta.width, height: canvasMeta.height,
      layers: [{
        actorId: String(localIdRef.current ?? 0),
        background: layer,
        foreground: layer,
      }],
    };
    await painterRef.current.applyCheckpoint(checkpoint);
  }, [canvasMeta, clearCursors]);

  /**
   * What this client believed, for a report about why it was wrong.
   *
   * Every field here is a ref in this closure and nothing outside the tab can
   * read any of them, which is why a canvas that came out wrong could only be
   * reasoned about from the outside last time.
   */
  const diagnosticContext = useCallback(
    (reason: string, detail?: string): DiagnosticContext => ({
      reason,
      detail,
      localId: localIdRef.current,
      appliedSequence: appliedSequenceRef.current,
      expectedSequence: expectedSequenceRef.current,
      lastSeq: lastSeqRef.current,
      catchingUp: isCatchingUpRef.current,
      settled: painterRef.current?.isSynchronizationSettled() ?? false,
    }),
    [],
  );

  const handleResetPoint = useCallback(async (
    baseSequence: number, sequence: number | undefined, snapshotCount: number,
  ) => {
    const painter = painterRef.current;
    if (!painter) return;
    // The snapshots of this checkpoint all carry `baseSequence`, and knowing
    // how many there are is what lets the drain tell whole from half-arrived.
    snapshotCountsRef.current.set(baseSequence, snapshotCount);
    await drainCanonical();
    // A client that was in the room for the checkpoint is already past its
    // base and has nothing to restore: the server broadcasts the point alone,
    // never the snapshots, so the drain above had none to apply and none to
    // wait for. A client replaying history has just been given them, and if
    // the drain could not put them on the canvas then advancing here would
    // step the position over every message the checkpoint stands for -- and
    // leave this client certain it was caught up on a canvas missing all of
    // them, which is exactly how a session lost its drawing and told nobody.
    //
    // Staying put instead leaves the position behind `lastSeq`, which is what
    // `verifyCanonicalPosition` already watches for: the socket closes on the
    // gap and the replay is asked for again.
    if (appliedSequenceRef.current < baseSequence) {
      console.error(
        `Checkpoint at ${baseSequence} could not be applied; leaving the canonical position behind`,
      );
      reportDiagnostics(
        getSessionId(),
        painter,
        diagnosticContext(
          "checkpoint-not-applied",
          `base ${baseSequence}, ${snapshotCount} snapshots announced`,
        ),
      );
      return;
    }
    await painter.compactCanonicalHistory(baseSequence);
    if (sequence !== undefined) {
      appliedSequenceRef.current = sequence;
      expectedSequenceRef.current = sequence + 1;
    }
  }, [drainCanonical, diagnosticContext]);

  const verifyCanonicalPosition = useCallback(async (): Promise<boolean> => {
    await drainCanonical();
    return appliedSequenceRef.current >= lastSeqRef.current;
  }, [drainCanonical]);

  const canResumeCanonicalPosition = useCallback((): boolean =>
    painterRef.current?.isSynchronizationSettled() ?? false,
  []);

  /**
   * The server's 1-byte session id is the only name this canvas answers to:
   * every canonical message carries it, so the optimistic fork has to be
   * stamped with it too. Sending it on to the painter here is what keeps one
   * person from becoming two -- one identity for the pixels drawn ahead of the
   * server, another for the echo that confirms them.
   *
   * Safe to do at this point: WELCOME precedes the catch-up that gates
   * drawing, and `onLocalOperation` refuses to emit while `localIdRef` is
   * null, so no local operation carries the placeholder identity.
   */
  const handleWelcome = useCallback((sessionId: number) => {
    localIdRef.current = sessionId;
    painterRef.current?.setLocalActorId(String(sessionId));
  }, []);

  const handleSynchronizationError = useCallback((error: Error | null) => {
    setSynchronizationError(error?.message ?? null);
    // Only the failures, not the clearing of one.
    if (error) {
      reportDiagnostics(
        getSessionId(),
        painterRef.current,
        diagnosticContext("synchronization-error", error.message),
      );
    }
  }, [diagnosticContext]);

  const handleSessionEnded = useCallback((postUrl: string) => {
    setSessionEnding(true);
    window.location.assign(postUrl);
  }, []);

  const handleSessionExpired = useCallback(() => {
    setSessionExpired(true);
    painterRef.current?.setInteractionEnabled(false);
  }, []);

  /**
   * Whether a checkpoint made right now would describe canonical state.
   *
   * The same two conditions `handleResetRequest` waits for below, asked without
   * the waiting: a canvas still holding an optimistic fork, or one behind the
   * canonical position, would checkpoint pixels the room has not agreed on.
   * Answering the server's query on this rather than on willingness is what
   * stops a room from picking a client that will silently give up.
   */
  const canUploadCheckpoint = useCallback((): boolean => {
    const painter = painterRef.current;
    if (!painter || localIdRef.current === null) return false;
    return (
      painter.isSynchronizationSettled() &&
      appliedSequenceRef.current >= lastSeqRef.current
    );
  }, []);

  const handleResetRequest = useCallback(async () => {
    const painter = painterRef.current;
    const ws = wsRef.current;
    const localId = localIdRef.current;
    if (!painter || !ws || ws.readyState !== WebSocket.OPEN || localId === null) return;
    // A reset checkpoint must describe confirmed canonical state, never a
    // pointer gesture or optimistic fork the server has not sequenced yet.
    for (let attempt = 0; attempt < 40; attempt++) {
      await drainCanonical();
      if (
        painter.isSynchronizationSettled() &&
        appliedSequenceRef.current >= lastSeqRef.current
      ) break;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    if (
      !painter.isSynchronizationSettled() ||
      appliedSequenceRef.current < lastSeqRef.current
    ) return;
    const checkpoint = await painter.exportCheckpoint(appliedSequenceRef.current);
    // Each snapshot is stamped with the participant whose layer it is, not
    // with the uploader: one client uploads the whole canvas on everyone's
    // behalf, and the owner byte is what puts each pair back where it came
    // from.
    const snapshots = await Promise.all(
      checkpoint.layers.flatMap((entry) => [
        encodeSnapshot(localId, Number(entry.actorId), "background", entry.background),
        encodeSnapshot(localId, Number(entry.actorId), "foreground", entry.foreground),
      ]),
    );
    ws.send(encodeResetBegin(checkpoint.sequence, snapshots.length));
    snapshots.forEach((snapshot) => ws.send(snapshot));
  // wsRef is created by the WebSocket hook below and is stable for its lifetime.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [drainCanonical]);

  /**
   * The canonical position this client's last uploaded preview described.
   *
   * Zero is "nothing drawn yet", so a room nobody has touched shows no card
   * picture rather than a white rectangle, and a client joining a room that is
   * already underway refreshes it once instead of waiting for the next stroke.
   */
  const previewSequenceRef = useRef(0);
  /** So a slow render cannot have the next tick start a second one on top. */
  const previewInFlightRef = useRef(false);

  const refreshPreview = useCallback(async () => {
    const painter = painterRef.current;
    if (!painter || previewInFlightRef.current) return;
    // The same readiness a checkpoint needs, for the same reason: a canvas
    // still catching up, or holding an optimistic fork, is not what the room
    // looks like. Unlike a checkpoint this one does not wait for it -- there
    // is another tick along shortly, and a preview is never worth blocking on.
    //
    // `canUploadCheckpoint` alone is not enough during a replay: it compares
    // the applied position against the highest sequence *seen*, which while
    // history is streaming is the replay's own cursor rather than its target.
    // Every tick mid-replay therefore looks caught up, and the lobby would get
    // whatever fraction of the drawing had arrived by then.
    if (isCatchingUpRef.current || !canUploadCheckpoint()) return;
    // Nothing has moved since this client last uploaded, so whatever is on the
    // lobby is already current. Other clients keep their own count and will
    // each refresh once after their own last upload, which is what eventually
    // settles an idle room into uploading nothing at all.
    //
    // Different from, rather than ahead of: a checkpoint or a replaced history
    // can put the canonical position *behind* where this client last uploaded
    // from, and a room that had been drawn in would then never refresh again.
    const sequence = appliedSequenceRef.current;
    if (sequence === previewSequenceRef.current) return;
    previewInFlightRef.current = true;
    try {
      const uploaded = await refreshSessionPreview(getSessionId(), () =>
        painter.exportPng(),
      );
      if (uploaded) previewSequenceRef.current = sequence;
    } catch {
      // A preview is decoration on somebody else's page. Losing one must never
      // reach the person drawing.
    } finally {
      previewInFlightRef.current = false;
    }
  }, [canUploadCheckpoint]);

  // Stops as the session winds down: the canvas is about to become a post, and
  // the room's preview is deleted with the rest of its state. One that landed
  // after that would be about a session the lobby no longer lists.
  useEffect(() => {
    if (!canvasMeta || sessionEnding) return;
    const timer = window.setInterval(() => {
      void refreshPreview();
    }, PREVIEW_ATTEMPT_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [canvasMeta, sessionEnding, refreshPreview]);

  const { wsRef, connect } = useWebSocket({
    canvasMeta, userIdRef, userLoginNameRef, localUserJoinTimeRef,
    participantsRef, localIdRef, lastSeqRef, shouldConnectRef,
    catchupTimeoutRef, processingMessageRef, isCatchingUpRef,
    setConnectionState, setIsCatchingUp,
    setSyncProgress,
    onSynchronizationError: handleSynchronizationError,
    createOrUpdateCursor, hideCursor,
    addParticipant, clearParticipants,
    addChatMessage,
    handleResetRequest, canUploadCheckpoint, onReconnectCanvas, onCanvasMessage,
    onWelcome: handleWelcome,
    onResetPoint: handleResetPoint,
    verifyCanonicalPosition,
    canResumeCanonicalPosition,
    onSessionEnded: handleSessionEnded,
    onSessionExpired: handleSessionExpired,
  });

  const initializeApp = useCallback(async () => {
    try {
      setInitializationError(null);
      const sessionId = getSessionId();
      const authResponse = await fetch("/api/auth", { credentials: "include" });
      if (authResponse.status === 401) {
        window.location.href = `/login?next=${encodeURIComponent(window.location.pathname)}`;
        return;
      }
      if (!authResponse.ok) throw new Error(`Auth failed: ${authResponse.status}`);
      const auth = await authResponse.json();
      userIdRef.current = auth.user_id;
      userLoginNameRef.current = auth.login_name;
      localeRef.current = auth.preferred_locale || undefined;
      const response = await fetch(`/collaboration/${sessionId}/meta`, { credentials: "include" });
      if (!response.ok) throw new Error(`Failed to fetch collaboration meta: ${response.status}`);
      const meta: CollaborationMeta = await response.json();
      if (meta.savedPostId) {
        window.location.href = `/@${meta.ownerLoginName}/${meta.savedPostId}`;
        return;
      }
      if (meta.currentUserCount >= meta.maxUsers && meta.ownerId !== auth.user_id) {
        setRoomFullError({ currentUserCount: meta.currentUserCount, maxUsers: meta.maxUsers });
        return;
      }
      document.title = `Oeee Cafe - ${meta.title?.trim() || t`No Title`}`;
      setCanvasMeta(meta);
      shouldConnectRef.current = true;
    } catch (error) {
      setInitializationError(error instanceof Error ? error.message : String(error));
      setAuthError(true);
    }
    // `t` is deliberately absent: activating a locale hands out a new one even
    // when the locale is unchanged, and the painter activates one as it mounts.
    // See utils/linguiChurn.browser.test.tsx in neo-cucumber.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (joinedRef.current) return;
    joinedRef.current = true;
    void initializeApp();
  }, [initializeApp]);
  // Opens the socket once the session is known. `connect` is stable on
  // purpose: this effect must fire when the session arrives and at no other
  // time, or a render becomes a reconnect. Everything after the first
  // connection -- a dropped socket, a redeploy -- comes back through the
  // hook's own backoff.
  useEffect(() => {
    if (canvasMeta && painterRef.current && shouldConnectRef.current) connect();
  }, [canvasMeta, connect]);
  useEffect(() => () => { shouldConnectRef.current = false; wsRef.current?.close(); }, [wsRef]);

  const downloadPng = useCallback(async () => {
    const png = await painterRef.current?.exportPng();
    if (png) downloadBlob(png, "collaboration.png");
  }, []);

  const saveCollaborativeDrawing = useCallback(async () => {
    if (isSaving || !painterRef.current) return;
    setIsSaving(true);
    setSessionEnding(true);
    painterRef.current.setInteractionEnabled(false);
    try {
      const socket = wsRef.current;
      if (!socket || socket.readyState !== WebSocket.OPEN) {
        throw new Error("Reconnect before saving the collaborative session");
      }
      for (let attempt = 0; attempt < 40; attempt++) {
        await drainCanonical();
        if (
          painterRef.current.isSynchronizationSettled() &&
          appliedSequenceRef.current >= lastSeqRef.current
        ) break;
        await new Promise((resolve) => setTimeout(resolve, 100));
      }
      if (
        !painterRef.current.isSynchronizationSettled() ||
        appliedSequenceRef.current < lastSeqRef.current
      ) {
        throw new Error("The shared drawing is still synchronizing; please try again");
      }
      const png = await painterRef.current.exportPng();
      const response = await fetch(`/collaborate/${getSessionId()}`, {
        method: "POST", body: png, headers: { "Content-Type": "image/png" }, credentials: "include",
      });
      if (!response.ok) throw new Error(`Failed to save drawing: ${response.status}`);
      const result = await response.json();
      if (socket.readyState !== WebSocket.OPEN) {
        throw new Error("The session was saved, but the connection closed before finalization; reload to continue");
      }
      // The server echoes END_SESSION only after accepting the authoritative
      // lifecycle transition. Navigation happens in handleSessionEnded.
      socket.send(encodeEndSession(userIdRef.current, result.post_url));
      // The post above is already committed, so a confirmation that never
      // comes back must not strand the owner on a session that is over.
      window.setTimeout(() => window.location.assign(result.post_url), SAVE_CONFIRMATION_TIMEOUT_MS);
    } catch (error) {
      alert(error instanceof Error ? error.message : String(error));
      setIsSaving(false);
      setSessionEnding(false);
    }
  }, [drainCanonical, isSaving, wsRef]);

  const isOwner = canvasMeta?.ownerId === userIdRef.current;

  useEffect(() => {
    const painter = painterRef.current;
    const saveButton = saveButtonRef.current;
    const painterElement = painterElementRef.current;
    if (!painter || !painterElement) return;
    let proxy: HTMLButtonElement | null = null;
    let helpProxy: HTMLButtonElement | null = null;
    let helpButton: HTMLButtonElement | null = null;
    let cancelled = false;

    void painter.ready.then(() => {
      if (cancelled) return;
      const colorInput = painterElement.querySelector<HTMLInputElement>(
        'input[type="color"]',
      );
      const extraTools = colorInput?.parentElement;
      helpButton = painterElement.querySelector<HTMLButtonElement>(
        'button[aria-label="Keyboard shortcuts"]',
      );
      if (!extraTools || !helpButton) return;

      helpProxy = document.createElement("button");
      helpProxy.type = "button";
      helpProxy.className = NEO_PANEL_BUTTON;
      helpProxy.textContent = t`Help`;
      helpProxy.title = helpButton.title;
      helpProxy.setAttribute(
        "aria-label",
        helpButton.getAttribute("aria-label") ?? t`Help`,
      );
      helpProxy.addEventListener("click", () => helpButton?.click());

      extraTools.append(helpProxy);
      helpButton.hidden = true;
      if (isOwner && saveButton) {
        proxy = document.createElement("button");
        proxy.type = "button";
        proxy.className = NEO_PANEL_BUTTON;
        proxy.textContent = saveButton.textContent;
        proxy.disabled = saveButton.disabled;
        proxy.addEventListener("click", () => saveButton.click());
        extraTools.append(proxy);
        saveButton.hidden = true;
        saveProxyRef.current = proxy;
      }
    });

    return () => {
      cancelled = true;
      helpProxy?.remove();
      proxy?.remove();
      if (helpButton) helpButton.hidden = false;
      if (saveButton) saveButton.hidden = false;
      if (saveProxyRef.current === proxy) saveProxyRef.current = null;
    };
  }, [isOwner, t]);

  useEffect(() => {
    const proxy = saveProxyRef.current;
    if (!proxy) return;
    proxy.disabled = isSaving;
    proxy.textContent = isSaving ? t`Saving...` : t`Save to Gallery`;
  }, [isSaving, t]);

  return <>
    <div className="w-full app-container flex flex-col">
      <InitializationErrorModal isOpen={!!initializationError} errorMessage={initializationError ?? ""} onRetry={() => location.reload()} />
      <LoadingModal isOpen={!canvasMeta && !initializationError} />
      <AuthErrorModal isOpen={authError} onGoToLobby={() => { location.href = "/collaborate"; }} />
      <RoomFullModal isOpen={!!roomFullError} currentUserCount={roomFullError?.currentUserCount ?? 0} maxUsers={roomFullError?.maxUsers ?? 0} onGoToLobby={() => { location.href = "/collaborate"; }} onRetry={() => location.reload()} />
      {canvasMeta && <SessionHeader canvasMeta={canvasMeta} connectionState={connectionState} isCatchingUp={isCatchingUp} />}
      <div className="relative flex-1 overflow-hidden">
        {/*
          The chat is the toolbox's neighbour, so it is one of the painter's
          own windows: the same face, the same bevel, the same handle, and it
          moves, sizes and sits at the same height. All of that comes from
          neo-cucumber rather than from a copy of its values kept here, which
          is the only version of this that stays true.

          The corner is an element rather than CSS `resize` because `resize`
          draws no grabber on a touch browser -- on Chrome for Android there
          was nothing there to find.
        */}
        <div
          ref={chatFrameRef}
          className={`${NEO_PANEL} fixed z-40 flex flex-col overflow-hidden shadow-lg`}
          style={{
            left: `${chatPosition.x}px`,
            top: `${chatPosition.y}px`,
            width: `${chatSize.width}px`,
            height: `${chatSize.height}px`,
          }}
        >
          <div ref={chatHandleRef} className={NEO_TITLEBAR_HANDLE}>
            <span className={NEO_TITLEBAR_DOT} />
            <span className={NEO_TITLEBAR_DOT} />
            <span className={NEO_TITLEBAR_DOT} />
          </div>
          <Chat wsRef={wsRef} userId={userIdRef.current} participants={participants} connectionState={connectionState} onChatMessage={noopChatMessage} onAddMessage={holdChatAddMessage} />
          <div ref={chatResizeRef} aria-hidden="true" className={NEO_RESIZE_HANDLE}>
            <span className={NEO_RESIZE_GRIP} />
          </div>
        </div>
        <div ref={painterElementRef} className="h-full w-full" />
        <ConnectionStatusModal isCatchingUp={isCatchingUp} connectionState={connectionState} syncProgress={syncProgress} synchronizationError={synchronizationError} onReconnect={() => location.reload()} onDownloadPNG={downloadPng} />
        {isOwner && <button ref={saveButtonRef} type="button" disabled={isSaving} onClick={() => setSaveConfirmation(true)} className={`${NEO_BUTTON} absolute bottom-4 right-12 z-50`}>{isSaving ? <Trans>Saving...</Trans> : <Trans>Save to Gallery</Trans>}</button>}
        <SaveConfirmationModal
          isOpen={saveConfirmation}
          onCancel={() => setSaveConfirmation(false)}
          onConfirm={() => { setSaveConfirmation(false); void saveCollaborativeDrawing(); }}
        />
        <SessionEndingModal isOpen={sessionEnding} />
      </div>
    </div>
    <SessionExpiredModal isOpen={sessionExpired} isOwner={!!isOwner} canvasMeta={canvasMeta} isSaving={isSaving} onClose={() => {}} onSaveToGallery={saveCollaborativeDrawing} onDownloadPNG={downloadPng} onReturnToLobby={() => { location.href = "/collaborate"; }} />
  </>;
}
