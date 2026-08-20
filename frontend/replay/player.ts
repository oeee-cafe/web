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

/** How long to wait before the message at `index`, at a given speed. */
export function gapBefore(entries: ArchivedEntry[], index: number, speed: number): number {
  if (index <= 0 || index >= entries.length) return 0;
  const gap = entries[index].at - entries[index - 1].at;
  if (!Number.isFinite(gap) || gap <= 0) return 0;
  return Math.min(gap, MAX_GAP_MS) / speed;
}

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
  let painter = options.painter;
  /** How many messages are on the canvas. -1 is blank. */
  let applied = -1;
  let playing = false;
  let speed = 1;
  let timer: number | null = null;
  let destroyed = false;
  /** Serialises seeks and steps: both apply operations, and two at once would
   * interleave them. */
  let chain: Promise<void> = Promise.resolve();

  const progress = () => options.onProgress?.(applied, playing);

  const applyThrough = async (target: number) => {
    for (let index = applied + 1; index <= target && !destroyed; index++) {
      await painter.applyCanonicalOperation(drawable[index].operation);
      applied = index;
    }
  };

  const step = () => {
    timer = null;
    if (!playing || destroyed) return;
    chain = chain.then(async () => {
      if (!playing || destroyed) return;
      const next = applied + 1;
      if (next >= drawable.length) {
        playing = false;
        progress();
        return;
      }
      await applyThrough(next);
      progress();
      if (!playing || destroyed) return;
      const wait = gapBefore(
        drawable.map((held) => held.entry),
        next + 1,
        speed,
      );
      timer = window.setTimeout(step, wait);
    });
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
        progress();
      });
      await chain;
    },
    play() {
      if (playing || destroyed) return;
      if (applied >= drawable.length - 1) return;
      playing = true;
      progress();
      step();
    },
    pause() {
      playing = false;
      if (timer !== null) {
        window.clearTimeout(timer);
        timer = null;
      }
      progress();
    },
    setSpeed(next: number) {
      speed = next > 0 ? next : 1;
    },
    destroy() {
      destroyed = true;
      playing = false;
      if (timer !== null) window.clearTimeout(timer);
    },
  };
}
