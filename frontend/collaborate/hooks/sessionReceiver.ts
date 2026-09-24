import {
  decodeMessage,
  MSG_TYPE,
  unwrapReplayBatch,
  unwrapSequenced,
  type DecodedMessage,
  isCanvasHistoryMessage,
} from "../binaryProtocol";
import { acceptedResumeSequence } from "../synchronization";
import type { SessionLink, SessionView, SyncProgress } from "./useWebSocket";

/**
 * What the room's frames do to this client, apart from the socket they came
 * in on.
 *
 * The socket hook opens connections, retries them and closes them; this is
 * everything that happens to a frame once it has arrived. It used to be a
 * five-hundred-line function built inside the hook's connect callback,
 * closed over the socket and a dozen refs, and rebuilt whenever any of the
 * callbacks it forwarded to changed identity. As an object it can be handed
 * frames directly: the resume decisions on REPLAY_START and CAUGHT_UP, the
 * step over an unreadable sequence, the close on a history that changed
 * under the client, are each a test of a few frames rather than of a page.
 */

/** One line of the session chat, as the receiver reports it. */
export interface ChatLine {
  id: string;
  type: "user" | "join" | "leave";
  userId: string;
  username: string;
  message: string;
  timestamp: number;
}

export interface ReceiverHandlers {
  onSynchronizationError: (error: Error | null) => void;
  createOrUpdateCursor: (userId: string, x: number, y: number, username: string) => void;
  hideCursor: (userId: string) => void;
  addParticipant: (userId: string, username: string, joinedAt: number, sessionId?: number) => void;
  clearParticipants: () => void;
  addChatMessage: (message: ChatLine) => void;
  /** The room is asking who can upload a checkpoint. */
  onResetQuery: () => void;
  /** This client answered first, so the checkpoint is its to upload. */
  onResetSelected: () => void;
  onReconnectCanvas: (reconnecting: boolean, resumeSequence: number | null) => Promise<void> | void;
  onCanvasMessage: (message: DecodedMessage, raw: Uint8Array, sequence?: number) => Promise<void>;
  /**
   * A sequenced frame this client cannot read: a message type or tool code
   * newer than it is, or a frame truncated on its way into the history. It
   * still holds its place in canonical order, and the position has to be able
   * to step over it.
   */
  onUnreadableSequence: (sequence: number) => Promise<void>;
  onWelcome: (sessionId: number) => void;
  onResetPoint: (baseSequence: number, sequence: number | undefined, snapshotCount: number) => Promise<void> | void;
  verifyCanonicalPosition: () => Promise<boolean>;
  onSessionEnded: (postUrl: string) => void;
  onSessionExpired: () => void;
}

/** As much of a WebSocket as a received frame can act on. */
export interface ReceiverSocket {
  close(code: number, reason: string): void;
}

/**
 * How often the catch-up readout may publish a new position.
 *
 * It is a progress number on a modal, and publishing it re-renders the whole
 * session view. Ten times a second is faster than anybody reads.
 */
const PROGRESS_PUBLISH_MS = 100;

/** The close code this client uses when it has to ask for the history again. */
const WS_CLOSE_RESYNC = 4000;

export class SessionReceiver {
  /** The identity of the history the canvas is on; null until the first frame. */
  historyId: string | null = null;
  private caughtUp = false;
  private replayTarget: number | null = null;
  private failed = false;
  /**
   * Set on a reconnect until the first frame says whether the server accepted
   * the resume position or fell back to a full replay.
   */
  private reconnectPending = false;
  /** Whether the connection's URL asked to resume at all. */
  private resumeRequested = false;
  /** 1-byte session id -> display name (from LAYERS), for remote cursors. */
  private readonly idNames = new Map<number, string>();
  /** uuid -> 1-byte session id, for presence events keyed by uuid. */
  private readonly uuidToId = new Map<string, number>();
  /**
   * When the catch-up readout last moved.
   *
   * A number on a progress readout, published at the rate a history replays.
   * This is a state update on the session view -- the header, the chat, the
   * modal -- and a join replaying five hundred messages was five hundred
   * renders of all of it, for a counter nobody can read faster than it moves.
   */
  private progressPublishedAt = -Infinity;

  private readonly link: SessionLink;
  private readonly publish: (change: Partial<SessionView>) => void;
  private readonly handlers: () => ReceiverHandlers;
  private readonly now: () => number;

  constructor(
    link: SessionLink,
    publish: (change: Partial<SessionView>) => void,
    handlers: () => ReceiverHandlers,
    now: () => number = () => performance.now(),
  ) {
    this.link = link;
    this.publish = publish;
    this.handlers = handlers;
    this.now = now;
  }

  /**
   * The position to ask the server to continue from, for the next connect,
   * or null for a full replay. Nothing has arrived on the new socket yet, so
   * the received position starts over from the same place.
   *
   * Resume from what was applied, not from what was received. A socket closed
   * over a sequence gap has received past the hole, and asking to continue
   * after the last one received would have the server skip the missing
   * operations for good -- then clear everything held beyond the hole, and
   * call the canvas caught up.
   */
  resumeFrom(applied: number | null): { historyId: string; afterSeq: number } | null {
    this.resumeRequested = false;
    if (applied === null || this.historyId === null) return null;
    this.link.lastSeq = applied;
    this.resumeRequested = true;
    return { historyId: this.historyId, afterSeq: applied };
  }

  /**
   * A socket has opened. On a reconnect the URL declared the last verified
   * canonical position, and the visible canvas is kept until the first
   * history identity says whether the server accepted it.
   */
  async opened(reconnecting: boolean): Promise<void> {
    this.reconnectPending = reconnecting;
    if (!reconnecting) {
      await this.handlers().onReconnectCanvas(false, null);
      this.link.lastSeq = 0;
      this.historyId = null;
    }
    this.caughtUp = false;
    this.replayTarget = null;
    this.failed = false;
    this.progressPublishedAt = -Infinity;
    this.handlers().onSynchronizationError(null);
    this.link.localId = null; // reassigned by WELCOME
    // Catching up: drawing is held until the server says CAUGHT_UP. A timer
    // could not distinguish an empty history from a delayed or truncated
    // replay.
    this.publish({
      catchingUp: true,
      progress: { phase: "joining", receivedSequence: 0, appliedSequence: 0, targetSequence: null },
    });
  }

  /**
   * A frame could not be applied. Continuing after a failed canvas operation
   * would make later sequence numbers appear healthy on a corrupt canvas, so
   * nothing more is taken from this connection.
   */
  fail(error: unknown): Error {
    const syncError = error instanceof Error ? error : new Error(String(error));
    this.failed = true;
    this.handlers().onSynchronizationError(syncError);
    this.publish({ catchingUp: true });
    return syncError;
  }

  /** One frame from the server, in arrival order. */
  async receive(socket: ReceiverSocket, data: ArrayBuffer | Blob): Promise<void> {
    if (this.failed) return;

    const arrayBuffer = data instanceof ArrayBuffer ? data : await data.arrayBuffer();

    // A replay batch is several sequenced messages at once, and each is
    // applied exactly as its own SEQUENCED frame would be. Only during a
    // replay: the server sends them at no other time, and a batch that
    // arrived live would be something else pretending.
    if (new Uint8Array(arrayBuffer, 0, 1)[0] === MSG_TYPE.REPLAY_BATCH) {
      if (!this.link.catchingUp) return;
      const batch = unwrapReplayBatch(arrayBuffer);
      if (!batch) {
        console.error("Unreadable replay batch; reconnecting");
        socket.close(WS_CLOSE_RESYNC, "unreadable replay batch");
        return;
      }
      for (const entry of batch.entries) {
        if (this.failed) return;
        await this.receiveFrame(socket, entry.payload, {
          historyId: batch.historyId, seq: entry.seq, payload: entry.payload,
        });
      }
      return;
    }

    await this.receiveFrame(socket, arrayBuffer, unwrapSequenced(arrayBuffer));
  }

  /**
   * One message, with its canonical position when it has one.
   *
   * History messages arrive wrapped in a SEQUENCED envelope carrying their
   * canonical position; the position is recorded only after the message has
   * been fully applied so lastSeq always describes the canvas state.
   */
  private async receiveFrame(
    socket: ReceiverSocket,
    frame: ArrayBuffer,
    sequenced: { historyId: string; seq: number; payload: ArrayBuffer } | null,
  ): Promise<void> {
    let arrayBuffer = frame;
    if (sequenced) {
      if (this.reconnectPending) {
        await this.settleResume(
          acceptedResumeSequence(
            this.resumeRequested,
            { historyId: this.historyId, sequence: this.link.lastSeq },
            sequenced.historyId,
            sequenced.seq,
            "entry",
          ),
          sequenced.historyId,
        );
      }
      if (this.historyId === null) {
        this.historyId = sequenced.historyId;
      } else if (this.historyId !== sequenced.historyId) {
        console.error("Canonical history identity changed; reconnecting");
        this.publish({ catchingUp: true });
        socket.close(WS_CLOSE_RESYNC, "canonical history changed");
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
      decoded && sequenced && !isCanvasHistoryMessage(decoded) && decoded.type !== "resetPoint"
        ? null
        : decoded;
    if (!message) {
      if (!sequenced) return;
      // Every client of this build rejects the same frame the same way, and
      // asking for it again would only bring it back: resuming from before it
      // is a reconnect loop. So it is stepped over as an operation that does
      // nothing, which is what the decoder's own policy already is -- an
      // operation applied wrongly is worse than one not applied at all.
      console.warn(`Stepping over unreadable canonical message ${sequenced.seq}`);
      await this.handlers().onUnreadableSequence(sequenced.seq);
      this.link.lastSeq = Math.max(this.link.lastSeq, sequenced.seq);
      if (!this.link.catchingUp && !(await this.handlers().verifyCanonicalPosition())) {
        this.publish({ catchingUp: true });
        socket.close(WS_CLOSE_RESYNC, "canonical sequence gap");
      }
      return;
    }
    const raw = new Uint8Array(arrayBuffer);

    if (this.link.catchingUp) {
      // Pointer metadata describes the present moment, not replay state.
      if (message.type === "movePointer" || message.type === "pointerup") return;
      await this.applyDuringCatchUp(socket, message, raw, sequenced?.seq);
      return;
    }

    await this.handle(socket, message, raw, sequenced?.seq);
    if (sequenced) {
      this.link.lastSeq = Math.max(this.link.lastSeq, sequenced.seq);
      if (!(await this.handlers().verifyCanonicalPosition())) {
        this.publish({ catchingUp: true });
        socket.close(WS_CLOSE_RESYNC, "canonical sequence gap");
      }
    }
  }

  /**
   * The first frame of a reconnect has said what the server made of the
   * resume position: the canvas is kept from `resumeSequence`, or cleared
   * for the full replay that follows.
   */
  private async settleResume(resumeSequence: number | null, historyId: string): Promise<void> {
    await this.handlers().onReconnectCanvas(true, resumeSequence);
    this.reconnectPending = false;
    if (resumeSequence === null) this.link.lastSeq = 0;
    this.historyId = historyId;
  }

  /**
   * One frame of the replay, applied in turn. The caller already runs these
   * one at a time, in arrival order, so there is nothing to queue: a frame is
   * applied the moment it is decoded, and catch-up ends once the server has
   * said so and the canvas is at the position it said.
   */
  private async applyDuringCatchUp(
    socket: ReceiverSocket, message: DecodedMessage, raw: Uint8Array, seq: number | undefined,
  ): Promise<void> {
    await this.handle(socket, message, raw, seq);

    if (seq !== undefined) {
      this.link.lastSeq = Math.max(this.link.lastSeq, seq);
      const now = this.now();
      if (now - this.progressPublishedAt >= PROGRESS_PUBLISH_MS) {
        this.progressPublishedAt = now;
        this.publish({ progress: {
          phase: "applying",
          receivedSequence: this.link.lastSeq,
          appliedSequence: seq,
          targetSequence: this.replayTarget,
        } });
      }
    }

    if (!this.caughtUp) return;
    // The server's word is not enough: a missing canonical sequence would
    // otherwise enable editing on an incomplete canvas.
    if (await this.handlers().verifyCanonicalPosition()) {
      const ready: SyncProgress = {
        phase: "ready",
        receivedSequence: this.link.lastSeq,
        appliedSequence: this.link.lastSeq,
        targetSequence: this.link.lastSeq,
      };
      this.publish({ catchingUp: false, progress: ready });
    } else {
      console.error("Canonical sequence gap after catch-up; reconnecting");
      socket.close(WS_CLOSE_RESYNC, "canonical sequence gap");
    }
  }

  private nameOf(sessionId: number): string {
    return this.idNames.get(sessionId) || `#${sessionId}`;
  }

  private async handle(
    socket: ReceiverSocket, message: DecodedMessage, raw: Uint8Array, seq: number | undefined,
  ): Promise<void> {
    const handlers = this.handlers();
    const link = this.link;
    switch (message.type) {
      // All canvas-affecting messages fold into the shared canonical canvas
      // history, which owns conflict resolution (local fork reconciliation)
      // and collaborative undo.
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
        await handlers.onCanvasMessage(message, raw, seq);

        // Show remote users' cursors at their latest drawing position.
        if (
          message.userId !== link.localId &&
          (message.type === "stroke" || message.type === "fill")
        ) {
          const point =
            message.type === "stroke"
              ? message.points[message.points.length - 1]
              : { x: message.x, y: message.y };
          if (point) {
            handlers.createOrUpdateCursor(
              String(message.userId), point.x, point.y, this.nameOf(message.userId),
            );
          }
        }
        break;
      }

      case "welcome": {
        console.log("Assigned session user id:", message.sessionId);
        link.localId = message.sessionId;
        handlers.onWelcome(message.sessionId);
        break;
      }

      case "resetPoint": {
        await handlers.onResetPoint(message.baseSeq, seq, message.snapshotCount);
        break;
      }

      case "replayStart": {
        this.replayTarget = message.lastSeq;
        if (this.reconnectPending) {
          const resumeSequence =
            this.resumeRequested
              && this.historyId === message.historyId
              && link.lastSeq === message.afterSeq
              ? link.lastSeq
              : null;
          await this.settleResume(resumeSequence, message.historyId);
        }
        this.publish({ progress: {
          phase: "receiving",
          receivedSequence: message.afterSeq,
          appliedSequence: message.afterSeq,
          targetSequence: message.lastSeq,
        } });
        break;
      }

      case "caughtUp": {
        if (this.reconnectPending) {
          await this.settleResume(
            acceptedResumeSequence(
              this.resumeRequested,
              { historyId: this.historyId, sequence: link.lastSeq },
              message.historyId,
              message.lastSeq,
              "caughtUp",
            ),
            message.historyId,
          );
        }
        if (this.historyId === null) this.historyId = message.historyId;
        if (this.historyId !== message.historyId) {
          socket.close(WS_CLOSE_RESYNC, "caught-up history mismatch");
          break;
        }
        link.lastSeq = message.lastSeq;
        this.replayTarget = message.lastSeq;
        this.caughtUp = true;
        this.publish({ progress: {
          phase: "applying",
          receivedSequence: link.lastSeq,
          appliedSequence: Math.min(link.lastSeq, seq ?? link.lastSeq),
          targetSequence: message.lastSeq,
        } });
        break;
      }

      case "pointerup": {
        // Hide cursor for remote users when they stop drawing.
        if (message.userId !== link.localId) handlers.hideCursor(String(message.userId));
        break;
      }

      case "movePointer": {
        // Not while catching up. A pointer position is where somebody is
        // right now, and during a replay it is neither: the positions
        // arriving are from whenever the history was made, and drawing them
        // puts other people's cursors on a canvas that has not finished being
        // rebuilt.
        if (link.catchingUp) break;
        if (message.userId !== link.localId) {
          handlers.createOrUpdateCursor(
            String(message.userId), message.x, message.y, this.nameOf(message.userId),
          );
        }
        break;
      }

      case "join": {
        // The participant is added on LAYERS, which carries the server's
        // ordering; here only the chat hears of it.
        handlers.addChatMessage({
          id: `${message.userId}-${message.timestamp}-join`,
          type: "join",
          userId: message.userId,
          username: message.username,
          message: `${message.username} joined`,
          timestamp: message.timestamp,
        });
        break;
      }

      case "leave": {
        handlers.addChatMessage({
          id: `${message.userId}-${message.timestamp}-leave`,
          type: "leave",
          userId: message.userId,
          username: message.username,
          message: `${message.username} left the session`,
          timestamp: message.timestamp,
        });
        // Hide the cursor, but keep the participant in the list.
        const sessionId = this.uuidToId.get(message.userId);
        if (sessionId !== undefined) handlers.hideCursor(String(sessionId));
        break;
      }

      case "chat": {
        handlers.addChatMessage({
          id: `${message.userId}-${message.timestamp}`,
          type: "user",
          userId: message.userId,
          username: message.username,
          message: message.message,
          timestamp: message.timestamp,
        });
        break;
      }

      case "layers": {
        // Replace the roster wholesale, so every client holds the server's
        // ordering. Already sorted on the server, verified here.
        handlers.clearParticipants();
        const sorted = [...message.participants].sort((a, b) => a.joinTimestamp - b.joinTimestamp);
        for (const participant of sorted) {
          handlers.addParticipant(
            participant.userId,
            participant.username,
            participant.joinTimestamp,
            participant.sessionId > 0 ? participant.sessionId : undefined,
          );
          if (participant.sessionId > 0) {
            this.idNames.set(participant.sessionId, participant.username);
            this.uuidToId.set(participant.userId, participant.sessionId);
          }
        }
        break;
      }

      case "resetRequest": {
        if (message.phase === "query") {
          // The room is asking who can upload a checkpoint. Answering is one
          // byte; the work only starts if we win.
          handlers.onResetQuery();
        } else {
          // We answered first, so it is ours to do.
          handlers.onResetSelected();
        }
        break;
      }

      case "endSession": {
        if (message.postUrl) {
          link.shouldConnect = false;
          handlers.onSessionEnded(message.postUrl);
        }
        break;
      }

      case "sessionExpired": {
        link.shouldConnect = false;
        this.publish({ connection: "disconnected" });
        handlers.onSessionExpired();
        break;
      }

      default: {
        console.log("Unknown message type:", message);
        break;
      }
    }
  }
}
