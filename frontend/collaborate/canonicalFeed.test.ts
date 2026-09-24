import { describe, expect, it } from "vitest";
import type { CanonicalPainterOperation, PainterCheckpoint } from "neo-cucumber";
import { CanonicalFeed, type CanonicalTarget } from "./canonicalFeed";

/**
 * The stream's bookkeeping, without a page or a painter.
 *
 * What is asserted here is order and position: which operations reach the
 * canvas, in what order, and where the position stands afterwards. The
 * pixels are the painter's business and have their own tests.
 */

const operation = (sequence: number): CanonicalPainterOperation => ({
  id: `#${sequence}`,
  actorId: "1",
  sequence,
  operation: { kind: "undo-boundary" },
});

const png = () => new Blob([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], { type: "image/png" });

const CANVAS = { width: 64, height: 48 };

/** A painter that only remembers what it was asked to do. */
class Recorder implements CanonicalTarget {
  readonly applied: string[] = [];
  readonly checkpoints: PainterCheckpoint[] = [];
  /** Lets a test hold an apply open, to overlap two drains. */
  gate: Promise<void> | null = null;

  async applyCanonicalOperation(op: CanonicalPainterOperation): Promise<void> {
    if (this.gate) await this.gate;
    this.applied.push(op.id);
  }

  async applyCheckpoint(checkpoint: PainterCheckpoint): Promise<void> {
    this.checkpoints.push(checkpoint);
    this.applied.push(`checkpoint@${checkpoint.sequence}`);
  }
}

const feed = () => new CanonicalFeed({ mayYield: () => false });

describe("the canonical feed", () => {
  it("applies operations in sequence order, whatever order they were held in", async () => {
    const painter = new Recorder();
    const stream = feed();
    stream.hold(operation(3));
    stream.hold(operation(1));
    stream.hold(operation(2));
    await stream.drain(painter, CANVAS);
    expect(painter.applied).toEqual(["#1", "#2", "#3"]);
    expect(stream.applied).toBe(3);
    expect(stream.expected).toBe(4);
  });

  it("holds everything past a gap until the gap is filled", async () => {
    const painter = new Recorder();
    const stream = feed();
    stream.hold(operation(1));
    stream.hold(operation(3));
    await stream.drain(painter, CANVAS);
    expect(painter.applied).toEqual(["#1"]);
    expect(stream.applied).toBe(1);

    stream.hold(operation(2));
    await stream.drain(painter, CANVAS);
    expect(painter.applied).toEqual(["#1", "#2", "#3"]);
    expect(stream.applied).toBe(3);
  });

  it("steps over an unreadable sequence without applying anything for it", async () => {
    const painter = new Recorder();
    const stream = feed();
    stream.hold(operation(1));
    stream.holdUnreadable(2);
    stream.hold(operation(3));
    await stream.drain(painter, CANVAS);
    expect(painter.applied).toEqual(["#1", "#3"]);
    expect(stream.applied).toBe(3);
  });

  it("applies a checkpoint only once every announced snapshot has arrived", async () => {
    const painter = new Recorder();
    const stream = feed();
    stream.holdSnapshot(100, "1", "foreground", png());
    stream.holdSnapshot(100, "1", "background", png());
    stream.holdSnapshot(100, "3", "foreground", png());
    await stream.drain(painter, CANVAS);
    expect(painter.checkpoints, "before the count is known").toHaveLength(0);

    stream.announceCheckpoint(100, 4);
    await stream.drain(painter, CANVAS);
    expect(painter.checkpoints, "three of four").toHaveLength(0);
    expect(stream.applied).toBe(0);

    stream.holdSnapshot(100, "3", "background", png());
    await stream.drain(painter, CANVAS);
    expect(painter.checkpoints).toHaveLength(1);
    expect(stream.applied).toBe(100);
    expect(stream.expected).toBe(101);
  });

  it("keys a checkpoint's layers by whose they are and stacks them by session id", async () => {
    const painter = new Recorder();
    const stream = feed();
    stream.announceCheckpoint(100, 4);
    // Uploaded by one client on behalf of two; arrives higher id first.
    stream.holdSnapshot(100, "3", "background", png());
    stream.holdSnapshot(100, "3", "foreground", png());
    stream.holdSnapshot(100, "1", "background", png());
    stream.holdSnapshot(100, "1", "foreground", png());
    await stream.drain(painter, CANVAS);
    const [checkpoint] = painter.checkpoints;
    expect(checkpoint.width).toBe(CANVAS.width);
    expect(checkpoint.height).toBe(CANVAS.height);
    expect(checkpoint.layers.map((layer) => layer.actorId)).toEqual(["1", "3"]);
  });

  it("jumps over held operations the checkpoint stands for, and applies those after it", async () => {
    const painter = new Recorder();
    const stream = feed();
    // Operations the checkpoint already contains, held because nothing
    // below them ever arrived.
    stream.hold(operation(98));
    stream.hold(operation(100));
    // And ones sequenced after its base.
    stream.hold(operation(101));
    stream.hold(operation(102));
    stream.announceCheckpoint(100, 2);
    stream.holdSnapshot(100, "1", "background", png());
    stream.holdSnapshot(100, "1", "foreground", png());
    await stream.drain(painter, CANVAS);
    expect(painter.applied).toEqual(["checkpoint@100", "#101", "#102"]);
    expect(stream.applied).toBe(102);
  });

  it("takes the lowest whole checkpoint when more than one is ahead", async () => {
    const painter = new Recorder();
    const stream = feed();
    for (const base of [200, 100]) {
      stream.announceCheckpoint(base, 2);
      stream.holdSnapshot(base, "1", "background", png());
      stream.holdSnapshot(base, "1", "foreground", png());
    }
    stream.hold(operation(150));
    await stream.drain(painter, CANVAS);
    expect(painter.applied).toEqual(["checkpoint@100", "checkpoint@200"]);
    expect(stream.applied).toBe(200);
  });

  it("forgets what it held on a restart and counts on from the given position", async () => {
    const painter = new Recorder();
    const stream = feed();
    stream.hold(operation(5));
    stream.announceCheckpoint(10, 2);
    stream.holdSnapshot(10, "1", "background", png());
    stream.restart(4);
    expect(stream.applied).toBe(4);
    expect(stream.expected).toBe(5);
    stream.holdSnapshot(10, "1", "foreground", png());
    await stream.drain(painter, CANVAS);
    expect(painter.applied, "neither the operation nor the half checkpoint survived").toEqual([]);
  });

  it("runs one drain at a time, so two callers cannot interleave", async () => {
    const painter = new Recorder();
    const stream = feed();
    let release!: () => void;
    painter.gate = new Promise((resolve) => { release = resolve; });
    stream.hold(operation(1));
    stream.hold(operation(2));
    const first = stream.drain(painter, CANVAS);
    stream.hold(operation(3));
    const second = stream.drain(painter, CANVAS);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(painter.applied, "the first apply is still held open").toEqual([]);
    release();
    await Promise.all([first, second]);
    expect(painter.applied).toEqual(["#1", "#2", "#3"]);
  });

  it("yields between operations once over budget, and only when it may", async () => {
    let clock = 0;
    const yields: number[] = [];
    let allowed = true;
    const stream = new CanonicalFeed({
      mayYield: () => allowed,
      yieldToInput: async () => { yields.push(clock); },
      budgetMs: 5,
      now: () => clock,
    });
    const painter = new Recorder();
    const slow: CanonicalTarget = {
      async applyCanonicalOperation(op) {
        clock += 3;
        await painter.applyCanonicalOperation(op);
      },
      applyCheckpoint: (checkpoint) => painter.applyCheckpoint(checkpoint),
    };
    for (let sequence = 1; sequence <= 4; sequence++) stream.hold(operation(sequence));
    await stream.drain(slow, CANVAS);
    // 3ms, 6ms (yield), 9ms, 12ms (yield): once per budget, not per operation.
    expect(yields).toEqual([6, 12]);
    expect(painter.applied).toHaveLength(4);

    allowed = false;
    yields.length = 0;
    for (let sequence = 5; sequence <= 8; sequence++) stream.hold(operation(sequence));
    await stream.drain(slow, CANVAS);
    expect(yields, "not while catching up").toEqual([]);
    expect(painter.applied).toHaveLength(8);
  });
});
