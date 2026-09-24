import type { CanonicalPainterOperation, PainterCheckpoint } from "neo-cucumber";

/**
 * The canonical stream between the socket and the canvas.
 *
 * Messages arrive with a sequence number and are applied in sequence order,
 * whatever order the decoder finishes them in. A checkpoint is the one legal
 * jump: a group of snapshots, one pair of layers per participant, all
 * carrying the base sequence they stand for, followed by a RESET_POINT that
 * says how many there were. Only a whole group replaces the canvas; a partial
 * one is held until it is whole, and the position stays where it was.
 *
 * This used to be six refs and a loop inside the session view, which made
 * it a claim about the page rather than about a stream of frames. The
 * painter and the canvas size are handed to `drain` by the caller, so the
 * bookkeeping can be checked without either.
 */

/** Where released operations go: the painter, on the page. */
export interface CanonicalTarget {
  applyCanonicalOperation(operation: CanonicalPainterOperation): Promise<void>;
  applyCheckpoint(checkpoint: PainterCheckpoint): Promise<void>;
}

export interface CanvasSize {
  width: number;
  height: number;
}

export interface CanonicalFeedOptions {
  /**
   * Whether the drain may hand the thread back between operations.
   *
   * Not while catching up: the painter takes no input until the replay is
   * done, so there is nothing to be responsive to and yielding would only
   * make the wait longer.
   */
  mayYield?: () => boolean;
  yieldToInput?: () => Promise<void>;
  /** How long the drain may run before it yields; see `CANONICAL_BUDGET_MS`. */
  budgetMs?: number;
  now?: () => number;
}

type LayerName = "background" | "foreground";
type HeldPair = Partial<Record<LayerName, Blob>>;

/**
 * How long the drain may run before yielding.
 *
 * Canonical operations arrive faster than they can be applied whenever the
 * room is busy, and applying them is what the pointer competes with for the
 * main thread. A drain that ran to exhaustion would starve input, because
 * every `await` inside it is a microtask and microtasks are not a yield:
 * the queue drains completely before the browser is allowed to deliver the
 * next pointer event.
 *
 * Drawpile bounds the same work at about 0.2ms per batch, which it can
 * afford because its paint engine has a thread to itself and is emptied
 * again on the next tick. Ours has to share, so the budget is most of a
 * frame rather than a fraction of one, and what follows it is a real yield.
 */
export const CANONICAL_BUDGET_MS = 6;

/**
 * Hands the thread back long enough for input to be delivered.
 *
 * `scheduler.yield` resumes at a priority above an ordinary task, so the
 * drain picks up again ahead of anything incidental; without it a message
 * channel is the cheapest macrotask that still lets the browser run pending
 * input first. A `setTimeout` is not a substitute -- nested timeouts are
 * clamped to 4ms, which would cost more than the work being interrupted.
 */
export const yieldToInput = (): Promise<void> => {
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

export class CanonicalFeed {
  /** The last sequence on the canvas. */
  private appliedSequence = 0;
  /** The sequence the canvas is waiting for. */
  private expectedSequence = 1;
  /** Held until their turn; `null` holds the place of one nobody can read. */
  private readonly held = new Map<number, CanonicalPainterOperation | null>();
  /**
   * Snapshots of a pending checkpoint, by sequence then by participant.
   *
   * Every snapshot of one reset carries the same sequence, and a checkpoint
   * covers a layer pair per participant, so the group is only whole once
   * RESET_POINT has said how many snapshots to expect and that many have
   * arrived. Applying a partial group would blank whoever was still in
   * flight.
   */
  private readonly snapshots = new Map<number, Map<string, HeldPair>>();
  private readonly announced = new Map<number, number>();
  /** Drains run one at a time, in the order they were asked for. */
  private draining: Promise<void> = Promise.resolve();
  private readonly mayYield: () => boolean;
  private readonly yieldToInput: () => Promise<void>;
  private readonly budgetMs: number;
  private readonly now: () => number;

  constructor(options: CanonicalFeedOptions = {}) {
    this.mayYield = options.mayYield ?? (() => true);
    this.yieldToInput = options.yieldToInput ?? yieldToInput;
    this.budgetMs = options.budgetMs ?? CANONICAL_BUDGET_MS;
    this.now = options.now ?? (() => performance.now());
  }

  get applied(): number {
    return this.appliedSequence;
  }

  get expected(): number {
    return this.expectedSequence;
  }

  hold(operation: CanonicalPainterOperation): void {
    this.held.set(operation.sequence, operation);
  }

  /**
   * A sequence this client cannot read. It still holds its place in
   * canonical order, so the position steps over it as an operation that
   * does nothing.
   */
  holdUnreadable(sequence: number): void {
    this.held.set(sequence, null);
  }

  /**
   * One layer of one participant's pair, from the checkpoint at `sequence`.
   *
   * Keyed by whose layer it is, not who sent it: one client uploads the
   * whole checkpoint on everyone's behalf, so the sender is the same for
   * every snapshot in it. Keying by sender collapsed all of them onto one
   * participant, left the checkpoint one pair short of the count that says
   * it is whole, and so stopped it from ever being applied.
   */
  holdSnapshot(sequence: number, actorId: string, layer: LayerName, png: Blob): void {
    const owners = this.snapshots.get(sequence) ?? new Map<string, HeldPair>();
    const pair = owners.get(actorId) ?? {};
    pair[layer] = png;
    owners.set(actorId, pair);
    this.snapshots.set(sequence, owners);
  }

  /** How many snapshots the checkpoint at `baseSequence` has, from its RESET_POINT. */
  announceCheckpoint(baseSequence: number, snapshotCount: number): void {
    this.announced.set(baseSequence, snapshotCount);
  }

  /** Forgets everything held and counts on from `position`. */
  restart(position: number): void {
    this.held.clear();
    this.snapshots.clear();
    this.announced.clear();
    this.appliedSequence = position;
    this.expectedSequence = position + 1;
  }

  /**
   * Moves the position to a RESET_POINT the painter has compacted to. The
   * caller has checked that the checkpoint it stands for was applied.
   */
  advanceTo(sequence: number): void {
    this.appliedSequence = sequence;
    this.expectedSequence = sequence + 1;
  }

  /**
   * Applies everything that is next in line, in order, until something is
   * missing. Waits for any drain already running first, so two callers
   * cannot interleave their operations.
   */
  drain(target: CanonicalTarget, canvas: CanvasSize): Promise<void> {
    this.draining = this.draining.then(() => this.release(target, canvas));
    return this.draining;
  }

  private async release(target: CanonicalTarget, canvas: CanvasSize): Promise<void> {
    let deadline = this.now() + this.budgetMs;
    while (true) {
      const expected = this.expectedSequence;
      const operation = this.held.get(expected);
      if (operation !== undefined) {
        this.held.delete(expected);
        if (operation) await target.applyCanonicalOperation(operation);
        this.advanceTo(expected);
        if (this.mayYield() && this.now() >= deadline) {
          await this.yieldToInput();
          deadline = this.now() + this.budgetMs;
        }
        continue;
      }

      const checkpointSequence = this.wholeCheckpointFrom(expected);
      if (checkpointSequence === undefined) return;
      const owners = this.snapshots.get(checkpointSequence)!;
      await target.applyCheckpoint({
        sequence: checkpointSequence,
        width: canvas.width,
        height: canvas.height,
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
      this.snapshots.delete(checkpointSequence);
      this.announced.delete(checkpointSequence);
      for (const sequence of this.held.keys()) {
        if (sequence <= checkpointSequence) this.held.delete(sequence);
      }
      this.advanceTo(checkpointSequence);
    }
  }

  /**
   * The lowest checkpoint at or past `from` whose every announced snapshot
   * has arrived, which is the one legal jump over the operations it stands
   * for.
   */
  private wholeCheckpointFrom(from: number): number | undefined {
    let lowest: number | undefined;
    for (const [sequence, owners] of this.snapshots) {
      if (sequence < from) continue;
      if (lowest !== undefined && sequence >= lowest) continue;
      const expectedCount = this.announced.get(sequence);
      if (expectedCount === undefined) continue;
      let held = 0;
      for (const pair of owners.values()) {
        held += (pair.background ? 1 : 0) + (pair.foreground ? 1 : 0);
      }
      if (held >= expectedCount) lowest = sequence;
    }
    return lowest;
  }
}
