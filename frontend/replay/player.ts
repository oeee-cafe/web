/**
 * Playing a recorded session back through the painter that drew it.
 *
 * The one rule this has to obey: a replay renders through the same path a live
 * client does. `applyCanonicalOperation` is that path -- the same
 * `CanvasHistory`, the same `DrawingEngine`, the same decoder -- so a replay
 * that comes out different from the canvas it records is a bug in one of them
 * rather than a second opinion from a viewer that reimplemented the drawing.
 * A second renderer is how the `.pch` files would have started disagreeing
 * with NEO, and a collaborative canvas has a layer pair per participant and an
 * ordering discipline on top of that.
 */

import type { PainterHandle } from "neo-cucumber";
import {
  decodeMessage,
  decodePainterOperation,
  isCanvasHistoryMessage,
} from "../collaborate/binaryProtocol";
import type { ArchivedEntry } from "./archiveLog";

/**
 * The longest a replay waits between two messages.
 *
 * A room spends most of its life idle -- somebody thinking, somebody away --
 * and replaying that faithfully would mean watching nothing for minutes. The
 * gaps within a stroke are milliseconds and survive untouched; only the pauses
 * between are shortened.
 */
export const MAX_GAP_MS = 1200;

/** One recorded message, ready to apply, or null if it never touched a canvas. */
export function toCanonicalOperation(entry: ArchivedEntry) {
  const buffer = entry.payload.slice().buffer as ArrayBuffer;
  const message = decodeMessage(buffer);
  if (!message || !isCanvasHistoryMessage(message)) return null;
  const operation = decodePainterOperation(message);
  if (!operation || typeof message.userId !== "number") return null;
  return {
    // The archive holds what the room agreed on, so every entry is somebody's
    // confirmed work; nothing here is an echo of our own.
    id: `replay:${entry.seq}`,
    actorId: String(message.userId),
    sequence: entry.seq,
    operation,
  };
}

/**
 * The entries a replay actually draws.
 *
 * RESET_POINT is dropped: it tells a live client that history below a base was
 * squashed into snapshots it is about to receive, and a recording has no
 * snapshots because it kept the operations instead. Applying everything from
 * the first message reaches the same canvas by the longer road, which is the
 * road the recording is.
 */
export function drawableEntries(entries: ArchivedEntry[]) {
  return entries
    .map((entry) => ({ entry, operation: toCanonicalOperation(entry) }))
    .filter(
      (held): held is { entry: ArchivedEntry; operation: NonNullable<ReturnType<typeof toCanonicalOperation>> } =>
        held.operation !== null,
    );
}

/**
 * When each message is due, in milliseconds from the start of playback.
 *
 * Computed once, ahead of time, because it is the schedule rather than a
 * decision: a clock reads it, and reading it must not cost anything per
 * message. Recorded timestamps are server arrival times and can repeat -- a
 * burst sequenced inside one millisecond is due all at once, which is what it
 * was.
 */
export function timeline(entries: ArchivedEntry[]): number[] {
  const offsets = new Array<number>(entries.length);
  let at = 0;
  for (let index = 0; index < entries.length; index++) {
    if (index > 0) {
      const gap = entries[index].at - entries[index - 1].at;
      at += Number.isFinite(gap) && gap > 0 ? Math.min(gap, MAX_GAP_MS) : 0;
    }
    offsets[index] = at;
  }
  return offsets;
}

/**
 * How long one frame may spend applying messages.
 *
 * Half a frame at 60Hz, so the repaint the painter does on the same frame
 * still has room. A burst longer than this is spread over the frames after it
 * rather than blocking the page, which is what a player should do when it
 * cannot keep up.
 */
const FRAME_BUDGET_MS = 8;

export type ReplayHandle = {
  /** Applies everything up to and including `index`, from a blank canvas. */
  seek(index: number): Promise<void>;
  play(): void;
  pause(): void;
  setSpeed(speed: number): void;
  readonly length: number;
  destroy(): void;
};

type ReplayOptions = {
  painter: PainterHandle;
  entries: ArchivedEntry[];
  /** Called after every applied message so a page can show where it is. */
  onProgress?: (index: number, playing: boolean) => void;
  /** Rebuilds a blank painter, for seeking backwards. Applying operations is
   * the only way pixels get on this canvas, so going back means starting over;
   * a session is a few hundred messages and doing so is imperceptible. */
  remount: () => Promise<PainterHandle>;
};

export function createReplay(options: ReplayOptions): ReplayHandle {
  const drawable = drawableEntries(options.entries);
  const due = timeline(drawable.map((held) => held.entry));
  let painter = options.painter;
  /** How many messages are on the canvas. -1 is blank. */
  let applied = -1;
  let playing = false;
  let speed = 1;
  let frame: number | null = null;
  let destroyed = false;
  /** Where playback has reached on the timeline. Advanced by the wall clock
   * rather than by counting messages, so a frame that applies twenty of them
   * and a frame that applies none both leave it in the right place. */
  let virtual = 0;
  let lastFrameAt = 0;
  /** Serialises seeks against playback: both apply operations, and two at
   * once would interleave them. */
  let chain: Promise<void> = Promise.resolve();

  const progress = () => options.onProgress?.(applied, playing);

  const applyThrough = async (target: number) => {
    for (let index = applied + 1; index <= target && !destroyed; index++) {
      await painter.applyCanonicalOperation(drawable[index].operation);
      applied = index;
    }
  };

  /** Applies everything the clock has passed, within one frame's budget. */
  const pump = () => {
    chain = chain.then(async () => {
      if (!playing || destroyed) return;
      const until = performance.now() + FRAME_BUDGET_MS;
      while (playing && !destroyed) {
        const next = applied + 1;
        if (next >= drawable.length) {
          playing = false;
          break;
        }
        // Not due yet, and everything after it is later still.
        if (due[next] > virtual) break;
        await painter.applyCanonicalOperation(drawable[next].operation);
        applied = next;
        // Out of budget: the rest is due on the frames after this one, where
        // the clock will still be ahead of them.
        if (performance.now() >= until) break;
      }
      // Once per frame rather than once per message: the readout and the
      // scrubber are DOM, and a burst would otherwise write them a hundred
      // times over for one thing anybody could see.
      progress();
    });
    return chain;
  };

  const onFrame = (now: number) => {
    frame = null;
    if (!playing || destroyed) return;
    virtual += (now - lastFrameAt) * speed;
    lastFrameAt = now;
    void pump();
    frame = requestAnimationFrame(onFrame);
  };

  const stopFrames = () => {
    if (frame !== null) {
      cancelAnimationFrame(frame);
      frame = null;
    }
  };

  return {
    get length() {
      return drawable.length;
    },
    async seek(index: number) {
      const target = Math.max(-1, Math.min(index, drawable.length - 1));
      chain = chain.then(async () => {
        if (destroyed) return;
        if (target < applied) {
          painter = await options.remount();
          applied = -1;
        }
        await applyThrough(target);
        // The clock follows the canvas, so resuming carries on from here
        // rather than racing to catch up with where it had got to.
        virtual = target >= 0 ? due[target] : 0;
        progress();
      });
      await chain;
    },
    play() {
      if (playing || destroyed) return;
      if (applied >= drawable.length - 1) return;
      playing = true;
      lastFrameAt = performance.now();
      progress();
      frame = requestAnimationFrame(onFrame);
    },
    pause() {
      playing = false;
      stopFrames();
      progress();
    },
    setSpeed(next: number) {
      speed = next > 0 ? next : 1;
    },
    destroy() {
      destroyed = true;
      playing = false;
      stopFrames();
    },
  };
}
