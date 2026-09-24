import { describe, expect, it, vi } from "vitest";
import { CanvasHistory, MAX_FORK_FALLBEHIND } from "./canvasHistory";
import {
  BRUSH_TYPE,
  decodeMessage,
  encodeBezier,
  encodeEraseAll,
  encodeFill,
  encodeLine,
  encodeRegion,
  encodeStroke,
  encodeText,
  encodeUndo,
  encodeUndoPoint,
} from "../../../frontend/collaborate/binaryProtocol";
import { type DrawingEngine } from "../DrawingEngine";
import { WIRE_BRUSH_TYPES } from "../types/drawing";
import {
  isHistoryOperation,
  type HistoryStroke,
} from "../synchronization/historyOperations";

vi.mock("./canvasSnapshot", () => ({
  pngDataToLayer: vi.fn(async () => new Uint8ClampedArray(SIZE * SIZE * 4).fill(7)),
}));

const SIZE = 16;
const LOCAL = 1;
const REMOTE = 2;

type StrokeState = [[number, number], [number, number]] | null;

// Deterministic, observable stand-in for DrawingEngine: drawLine writes the
// red value at both endpoints (last writer wins), fill floods the layer's red
// channel, and every operation is logged so tests can detect replays.
class FakeEngine {
  imageWidth = SIZE;
  imageHeight = SIZE;
  /**
   * A layer pair per participant, exactly as the real engine keeps them.
   * `layers` stays the local participant's pair so the reads below still mean
   * "what the local user drew".
   */
  owners = new Map<string, Record<string, Uint8ClampedArray>>();
  layers: Record<string, Uint8ClampedArray>;

  constructor() {
    this.layers = this.layersFor(String(LOCAL));
  }

  layersFor(owner: string): Record<string, Uint8ClampedArray> {
    let pair = this.owners.get(owner);
    if (!pair) {
      pair = {
        foreground: new Uint8ClampedArray(SIZE * SIZE * 4),
        background: new Uint8ClampedArray(SIZE * SIZE * 4),
      };
      this.owners.set(owner, pair);
    }
    return pair;
  }

  ownerIds(): string[] {
    return [...this.owners.keys()];
  }

  /**
   * How many writes each participant's layers have taken. The real engine
   * counts these in its surfaces; here every write goes through the helpers
   * below, so they count instead.
   */
  generations = new Map<string, number>();
  layerGeneration(owner: string): number {
    return this.generations.get(owner) ?? 0;
  }
  noteWrite(owner: string): void {
    this.generations.set(owner, (this.generations.get(owner) ?? 0) + 1);
  }

  /** Renaming a pair, so savepoints holding it under the old name can follow. */
  private renameListeners = new Set<(from: string, to: string) => void>();
  onOwnerRenamed(listener: (from: string, to: string) => void): () => void {
    this.renameListeners.add(listener);
    return () => this.renameListeners.delete(listener);
  }
  renameOwner(from: string, to: string): void {
    const pair = this.owners.get(from);
    if (!pair) return;
    this.owners.delete(from);
    this.owners.set(to, pair);
    for (const listener of this.renameListeners) listener(from, to);
  }

  ops: string[] = [];
  // The mask the engine is currently pointed at. Logged with each op so a
  // test can tell whether the sender's mask survived the trip.
  maskType = 0;
  maskColor: [number, number, number] = [0, 0, 0];
  private strokeState: StrokeState = null;

  private maskTag(): string {
    return this.maskType === 0 ? "" : `:mask${this.maskType}(${this.maskColor.join(",")})`;
  }

  /** Which participant a buffer belongs to, for the write count. */
  private ownerOf(ctx: Uint8ClampedArray): string | undefined {
    for (const [owner, pair] of this.owners) {
      if (pair.foreground === ctx || pair.background === ctx) return owner;
    }
    return undefined;
  }

  drawLine(
    ctx: Uint8ClampedArray,
    x0: number,
    y0: number,
    x1: number,
    y1: number,
    _brushSize: number,
    _brushType: string,
    r: number
  ) {
    const owner = this.ownerOf(ctx);
    if (owner) this.noteWrite(owner);
    ctx[(y0 * SIZE + x0) * 4] = r;
    ctx[(y1 * SIZE + x1) * 4] = r;
    this.ops.push(`line:${x0},${y0}-${x1},${y1}:${r}${this.maskTag()}`);
    this.strokeState = [
      [x0, y0],
      [x1, y1],
    ];
  }

  doFloodFill(ctx: Uint8ClampedArray, _x: number, _y: number, r: number) {
    const owner = this.ownerOf(ctx);
    if (owner) this.noteWrite(owner);
    for (let i = 0; i < ctx.length; i += 4) {
      ctx[i] = r;
    }
    this.ops.push(`fill:${r}`);
  }

  // Which repaint each path asked for. Uploading a whole layer costs far more
  // than drawing the message did, so "did this ask for the whole canvas?" is
  // worth being able to assert.
  repaints: string[] = [];
  /** The same repaints, naming whose canvas each one was for. */
  ownedRepaints: string[] = [];
  queueLayerUpdate(layer: string, owner?: string) {
    // The real engine treats this as "written somewhere I could not see".
    this.noteWrite(owner ?? String(LOCAL));
    this.repaints.push(`all:${layer}`);
    this.ownedRepaints.push(`all:${owner ?? String(LOCAL)}/${layer}`);
  }
  queueLayerRegionUpdate(layer: string, owner?: string) {
    this.repaints.push(`region:${layer}`);
    this.ownedRepaints.push(`region:${owner ?? String(LOCAL)}/${layer}`);
  }

  // Region ops record what they were asked to do, and which buffers they were
  // pointed at, so a test can tell a fork apart from the live layers.
  clipboard: ImageData | null = null;
  getClipboard() {
    return this.clipboard;
  }
  setClipboard(data: ImageData | null) {
    this.clipboard = data;
  }
  applyRegionTool(
    tool: string,
    layer: string,
    rect: { x: number; y: number; width: number; height: number },
    color: { r: number; g: number; b: number; a: number },
    _size: number,
    targets?: { foreground: Uint8ClampedArray; background: Uint8ClampedArray }
  ) {
    if (tool === "copy") {
      // Stand-in for the copied pixels: the red value under the rectangle
      const buf = (targets ?? this.layers)[layer as "foreground"];
      this.clipboard = { pasted: buf[(rect.y * SIZE + rect.x) * 4] } as unknown as ImageData;
    } else if (tool === "paste") {
      const buf = (targets ?? this.layers)[layer as "foreground"];
      const value = (this.clipboard as unknown as { pasted: number } | null)?.pasted ?? -1;
      buf[(rect.y * SIZE + rect.x) * 4] = value;
    } else {
      const buf = (targets ?? this.layers)[layer as "foreground"];
      buf[(rect.y * SIZE + rect.x) * 4] = color.r;
    }
    this.ops.push(`region:${tool}:${targets ? "fork" : "live"}`);
  }
  eraseAll(layer: string, targets?: { foreground: Uint8ClampedArray; background: Uint8ClampedArray }) {
    (targets ?? this.layers)[layer as "foreground"].fill(0);
    this.ops.push(`eraseAll:${layer}`);
  }
  drawBezier(
    layer: string,
    points: number[],
    _size: number,
    _brush: string,
    color: { r: number; g: number; b: number; a: number },
    target?: Uint8ClampedArray
  ) {
    const buf = target ?? this.layers[layer];
    buf[(points[1] * SIZE + points[0]) * 4] = color.r;
    this.ops.push(`bezier:${points.join(",")}`);
  }
  drawText(
    layer: string,
    x: number,
    y: number,
    color: { r: number; g: number; b: number },
    _alpha: number,
    text: string,
    _fontSize: string,
    _family: string,
    into?: Uint8ClampedArray
  ) {
    const buf = into ?? this.layers[layer];
    buf[(y * SIZE + x) * 4] = color.r;
    this.ops.push(`text:${text}`);
  }

  getStrokeState(): StrokeState {
    return this.strokeState;
  }

  setStrokeState(state: StrokeState) {
    this.strokeState = state;
  }
}

function setup() {
  const engine = new FakeEngine();
  const history = new CanvasHistory(engine as unknown as DrawingEngine);
  history.setLocalUserId(LOCAL);
  return { engine, history };
}

/** An undo point and a fill, both confirmed, starting at `seq`. */
async function confirmedFill(
  history: CanvasHistory,
  author: number,
  target: number,
  x: number,
  y: number,
  r: number,
  seq: number,
) {
  await remote(history, encodeUndoPoint(author), seq);
  await remote(
    history,
    encodeFill(author, target, "background", x, y, r, 0, 0, 255),
    seq + 1,
  );
}

function red(engine: FakeEngine, x: number, y: number): number {
  return engine.layers.foreground[(y * SIZE + x) * 4];
}

/**
 * The red value in a named participant's foreground.
 *
 * Every participant paints into their own pair now, so a test that wants to
 * see what somebody else drew has to look in their layers rather than in the
 * local ones `red` reads.
 */
function redFor(
  engine: FakeEngine,
  owner: number | string,
  x: number,
  y: number
): number {
  return engine.layersFor(String(owner)).foreground[(y * SIZE + x) * 4];
}

function stroke(userId: number, x: number, y: number, r: number): ArrayBuffer {
  return encodeStroke(userId, userId, "foreground", 1, "solid", r, 0, 0, 255, [
    { x, y },
  ]);
}

function historyOperation(bytes: ArrayBuffer) {
  const message = decodeMessage(bytes);
  if (!message || !isHistoryOperation(message)) {
    throw new Error("expected a history operation");
  }
  return message;
}

function encodeHistoryStroke(stroke: HistoryStroke): ArrayBuffer {
  return encodeStroke(
    Number(stroke.userId), Number(stroke.targetOwner), stroke.layer, stroke.brushSize, stroke.brushType,
    stroke.color.r, stroke.color.g, stroke.color.b, stroke.color.a,
    stroke.points, stroke.mask,
  );
}

function flushStroke(history: CanvasHistory): ArrayBuffer {
  const stroke = history.flushLocalStroke();
  if (!stroke) throw new Error("expected a flushed stroke");
  const bytes = encodeHistoryStroke(stroke);
  history.commitLocalStroke(bytes, stroke);
  return bytes;
}

// Dispatches a locally generated non-stroke message (undo point, undo, fill)
function local(history: CanvasHistory, bytes: ArrayBuffer) {
  history.handleLocal(bytes, historyOperation(bytes));
}

// Draws one local segment and flushes it into a STROKE fork entry
function localStroke(
  history: CanvasHistory,
  x: number,
  y: number,
  r: number
): ArrayBuffer {
  history.addLocalSegment(
    "foreground",
    1,
    "solid",
    { r, g: 0, b: 0, a: 255 },
    x,
    y
  );
  return flushStroke(history);
}

// Delivers a message from the canonical server stream
async function remote(history: CanvasHistory, bytes: ArrayBuffer, seq?: number) {
  await history.handleRemote(new Uint8Array(bytes), historyOperation(bytes), seq);
}

describe("the optimistic fork's patience", () => {
  /**
   * The fork holds pixels only its author can see, waiting for the server to
   * echo them back. If the echoes stop coming it must not wait forever:
   * Drawpile counts how far behind the fork has fallen and gives up at
   * `MAX_FALLBEHIND`, because showing a canvas nobody else has is worse than
   * losing the work that was never acknowledged.
   */
  /**
   * The painter registers a local operation in the fork and then hands it to
   * the host to send, and a host declines to send while the socket is not
   * open, the session id is unknown, or a catch-up is running. The fork entry
   * stays either way, and nothing will ever echo it.
   *
   * The echo of the operation behind it is proof of that: one connection
   * delivers its messages in order and the server sequences them as they
   * arrive, so nothing that was actually sent can be overtaken. Treating this
   * as a divergence cleared the whole fork, which made every echo still in
   * flight diverge in turn -- a savepoint restore, a replay and a repaint of
   * every participant's layers per message, for the rest of the stroke.
   */
  it("drops the operations a send never carried, and rebuilds once", async () => {
    const { engine, history } = setup();
    history.setLocalUserId("1");

    const dot = (x: number, y: number, r: number) => ({
      kind: "stroke" as const,
      layer: "foreground" as const,
      brushSize: 1,
      brush: "solid" as const,
      color: { r, g: 0, b: 0, a: 255 },
      points: [{ x, y }],
      mask: { type: 0, r: 0, g: 0, b: 0 },
    });
    // What a controlled mount does: the interactive canvas paints, and the
    // history is told the operation happened.
    const paint = (id: string, x: number, y: number, r: number) => {
      engine.drawLine(engine.layersFor("1").foreground, x, y, x, y, 1, "solid", r);
      history.registerOptimisticOperation({ id, actorId: "1", operation: dot(x, y, r) });
    };
    const echo = (id: string, x: number, y: number, r: number, sequence: number) =>
      history.handleCanonicalOperation({
        id, actorId: "1", sequence, operation: dot(x, y, r),
      });

    paint("unsent", 2, 2, 90);
    paint("sent:1", 4, 4, 110);
    paint("sent:2", 6, 6, 130);

    engine.repaints.length = 0;
    await echo("sent:1", 4, 4, 110, 1);

    // The abandoned operation's pixels come off the canvas, because nobody
    // else ever had them; the rest of the fork keeps its own. The two that
    // remain are one gesture with no boundary between them, so the second
    // joins onto the first and owns the colour where they meet -- which is
    // exactly what the rebuild has to reproduce.
    expect(redFor(engine, "1", 2, 2)).toBe(0);
    expect(redFor(engine, "1", 4, 4)).toBe(130);
    expect(redFor(engine, "1", 6, 6)).toBe(130);
    expect(engine.repaints).toContain("all:foreground");

    // And the echo behind it is free: it is at the head of the fork again.
    engine.repaints.length = 0;
    await echo("sent:2", 6, 6, 130, 2);
    expect(engine.repaints).toEqual([]);
    expect(history.hasPendingLocal).toBe(false);
    expect(
      history.synchronizationTrace().flatMap((event) => event.action ?? []),
    ).toEqual(["abandoned", "echo"]);
  });

  it("gives up on a fork whose echoes never arrive", async () => {
    const { engine, history } = setup();
    history.handleLocalOperation({
      id: "never-echoed",
      actorId: String(LOCAL),
      operation: {
        kind: "stroke",
        layer: "foreground",
        brushSize: 1,
        brush: "solid",
        color: { r: 200, g: 0, b: 0, a: 255 },
        points: [{ x: 2, y: 2 }],
        mask: { type: 0, r: 0, g: 0, b: 0 },
      },
    });
    expect(history.hasPendingLocal).toBe(true);
    expect(redFor(engine, String(LOCAL), 2, 2)).toBe(200);

    // Somebody else keeps drawing, far away, so nothing forces a rollback on
    // its own account.
    for (let i = 0; i < MAX_FORK_FALLBEHIND - 1; i++) {
      await remote(history, stroke(REMOTE, 40, 40, 10), i + 1);
    }
    expect(history.hasPendingLocal).toBe(true);

    await remote(history, stroke(REMOTE, 40, 40, 10), MAX_FORK_FALLBEHIND);
    expect(history.hasPendingLocal).toBe(false);
    // The unacknowledged pixels went with it, which is the point: the canvas
    // now shows what the server actually said.
    expect(redFor(engine, String(LOCAL), 2, 2)).not.toBe(200);
  });

  it("forgets how far behind it was once the fork is confirmed", async () => {
    const { history } = setup();
    const operation = {
      kind: "stroke" as const,
      layer: "foreground" as const,
      brushSize: 1,
      brush: "solid" as const,
      color: { r: 90, g: 0, b: 0, a: 255 },
      points: [{ x: 5, y: 5 }],
      mask: { type: 0, r: 0, g: 0, b: 0 },
    };

    for (let round = 0; round < 3; round++) {
      history.handleLocalOperation({
        id: `op-${round}`,
        actorId: String(LOCAL),
        operation,
      });
      for (let i = 0; i < MAX_FORK_FALLBEHIND - 1; i++) {
        await remote(history, stroke(REMOTE, 40, 40, 10));
      }
      // The echo arrives, so the count starts again rather than carrying over
      // into the next stroke and tripping the bound on a healthy connection.
      await history.handleCanonicalOperation({
        id: `op-${round}`,
        actorId: String(LOCAL),
        operation,
        sequence: 1000 + round,
      });
      expect(history.hasPendingLocal).toBe(false);
    }
  });
});

describe("the synchronisation trace", () => {
  /**
   * A canvas that came out wrong is the worst failure here, and the operations
   * alone do not explain one: what matters is the order they arrived in
   * relative to local work and which branch of reconciliation each took, and
   * neither survives the moment it happens. Drawpile records the equivalent
   * for exactly this.
   */
  it("records which way each message went", async () => {
    const { history } = setup();
    const operation = {
      kind: "stroke" as const,
      layer: "foreground" as const,
      brushSize: 1,
      brush: "solid" as const,
      color: { r: 10, g: 0, b: 0, a: 255 },
      points: [{ x: 3, y: 3 }],
      mask: { type: 0, r: 0, g: 0, b: 0 },
    };

    // Nothing pending: straight through.
    await remote(history, stroke(REMOTE, 20, 20, 10), 1);

    // Ours, then its echo.
    history.handleLocalOperation({ id: "mine", actorId: String(LOCAL), operation });
    // Somebody else, nowhere near our pending pixels.
    await remote(history, stroke(REMOTE, 40, 40, 10), 2);
    // Somebody else painting into *our* pair, right on top of them --
    // drawing on another participant's layers is a thing this canvas allows,
    // and it is the case where a rollback is actually required. Two people at
    // the same coordinates on their own layers do not overlap at all.
    await remote(
      history,
      encodeStroke(REMOTE, LOCAL, "foreground", 1, "solid", 99, 0, 0, 255, [
        { x: 3, y: 3 },
      ]),
      3,
    );
    await history.handleCanonicalOperation({
      id: "mine",
      actorId: String(LOCAL),
      operation,
      sequence: 4,
    });

    const actions = history
      .synchronizationTrace()
      .map((event) => `${event.source}:${event.action ?? event.op}`);

    expect(actions).toEqual([
      "canonical:applied",
      "local:stroke",
      "canonical:concurrent",
      "canonical:replay",
      "canonical:echo",
    ]);
  });

  it("keeps the trace bounded, and hands back a copy", async () => {
    const { history } = setup();
    for (let i = 0; i < 700; i++) {
      await remote(history, stroke(REMOTE, 20, 20, 10), i + 1);
    }
    const trace = history.synchronizationTrace();
    expect(trace.length).toBeLessThanOrEqual(512);
    expect(trace.length).toBeGreaterThan(0);

    // A caller cannot reach in and edit what was recorded.
    trace[0].op = "tampered";
    expect(history.synchronizationTrace()[0].op).not.toBe("tampered");
  });
});

describe("canonical application", () => {
  it("reconciles public operation envelopes by operation id", async () => {
    const { engine, history } = setup();
    history.setLocalUserId("local");
    const operation = {
      kind: "stroke" as const,
      layer: "foreground" as const,
      brushSize: 1,
      brush: "solid" as const,
      color: { r: 75, g: 0, b: 0, a: 255 },
      points: [{ x: 3, y: 4 }],
      mask: { type: 0, r: 0, g: 0, b: 0 },
    };

    history.handleLocalOperation({ id: "op-1", actorId: "local", operation });
    expect(redFor(engine, "local", 3, 4)).toBe(75);
    expect(history.hasPendingLocal).toBe(true);

    await history.handleCanonicalOperation({
      id: "op-1",
      actorId: "local",
      operation,
      sequence: 1,
    });
    expect(redFor(engine, "local", 3, 4)).toBe(75);
    expect(history.hasPendingLocal).toBe(false);
  });

  it("applies remote strokes in server order, last writer wins", async () => {
    const { engine, history } = setup();
    await remote(history, stroke(REMOTE, 1, 1, 50), 1);
    await remote(history, stroke(REMOTE, 1, 1, 60), 2);
    expect(redFor(engine, REMOTE, 1, 1)).toBe(60);
    expect(engine.ops).toHaveLength(2);
  });

  it("chains a user's stroke points and resets on their undo point", async () => {
    const { engine, history } = setup();
    await remote(history, encodeUndoPoint(REMOTE), 1);
    await remote(
      history,
      encodeStroke(REMOTE, REMOTE, "foreground", 1, "solid", 50, 0, 0, 255, [
        { x: 1, y: 1 },
        { x: 3, y: 3 },
      ]),
      2
    );
    // Dot at first point, then a segment continuing from it. Segments run
    // new -> previous, as NEO draws them.
    expect(engine.ops).toEqual(["line:1,1-1,1:50", "line:3,3-1,1:50"]);

    await remote(history, encodeUndoPoint(REMOTE), 3);
    await remote(history, stroke(REMOTE, 5, 5, 60), 4);
    // New stroke starts with a dot, not a segment from (3,3)
    expect(engine.ops[2]).toBe("line:5,5-5,5:60");
  });

  it("applies a remote snapshot's decoded pixels", async () => {
    const { engine, history } = setup();
    const msg = {
      type: "snapshot" as const,
      userId: REMOTE,
      targetOwner: REMOTE,
      layer: "foreground" as const,
      pngData: new Uint8Array(0),
    };
    await history.handleRemote(new Uint8Array([0x02, 9, 9]), msg, 1);
    expect(redFor(engine, REMOTE, 0, 0)).toBe(7);
  });
});

describe("local fork reconciliation", () => {
  it("does not re-apply the echo of a local stroke", async () => {
    const { engine, history } = setup();
    const bytes = localStroke(history, 2, 2, 100);
    expect(red(engine, 2, 2)).toBe(100);
    expect(history.hasPendingLocal).toBe(true);

    await remote(history, bytes, 1);
    expect(history.hasPendingLocal).toBe(false);
    expect(engine.ops).toHaveLength(1); // applied exactly once
    expect(red(engine, 2, 2)).toBe(100);
  });

  it("applies a non-overlapping remote stroke without replaying the fork", async () => {
    const { engine, history } = setup();
    local(history, encodeUndoPoint(LOCAL));
    localStroke(history, 1, 1, 100);

    await remote(history, stroke(REMOTE, 10, 10, 200), 1);
    expect(red(engine, 1, 1)).toBe(100);
    expect(redFor(engine, REMOTE, 10, 10)).toBe(200);
    expect(engine.ops).toHaveLength(2); // no replay of the fork entry
  });

  it("keeps two participants' strokes on the same pixel apart, without replaying", async () => {
    const { engine, history } = setup();
    local(history, encodeUndoPoint(LOCAL));
    const mine = localStroke(history, 5, 5, 100);

    // The same pixel, but not the same layers: each participant paints into
    // their own pair, so there is nothing to reorder and the unconfirmed
    // local stroke never has to be rolled back and replayed.
    await remote(history, stroke(REMOTE, 5, 5, 200), 1);
    expect(red(engine, 5, 5)).toBe(100);
    expect(redFor(engine, REMOTE, 5, 5)).toBe(200);
    expect(engine.ops).toHaveLength(2); // local, remote -- no replay

    // Echoes arrive; the canvas must not change
    await remote(history, encodeUndoPoint(LOCAL), 2);
    await remote(history, mine, 3);
    expect(red(engine, 5, 5)).toBe(100);
    expect(redFor(engine, REMOTE, 5, 5)).toBe(200);
    expect(history.hasPendingLocal).toBe(false);
  });

  it("replays the fork when someone else draws on the layers we are drawing on", async () => {
    const { engine, history } = setup();
    local(history, encodeUndoPoint(LOCAL));
    const mine = localStroke(history, 5, 5, 100);

    // REMOTE aims at LOCAL's pair rather than their own. Now there really is
    // one set of pixels two people are writing, so canonical order decides
    // it: the remote stroke goes down first and the unconfirmed local one is
    // replayed on top.
    await remote(
      history,
      encodeStroke(REMOTE, LOCAL, "foreground", 1, "solid", 200, 0, 0, 255, [
        { x: 5, y: 5 },
      ]),
      1
    );
    expect(red(engine, 5, 5)).toBe(100);
    expect(engine.ops).toHaveLength(3); // local, remote (replay), local again

    // Echoes arrive; the canvas must not change
    await remote(history, encodeUndoPoint(LOCAL), 2);
    await remote(history, mine, 3);
    expect(red(engine, 5, 5)).toBe(100);
    expect(history.hasPendingLocal).toBe(false);
  });

  it("re-applies an unflushed open batch on top of a conflicting remote stroke", async () => {
    const { engine, history } = setup();
    local(history, encodeUndoPoint(LOCAL));
    history.addLocalSegment(
      "foreground",
      1,
      "solid",
      { r: 100, g: 0, b: 0, a: 255 },
      5,
      5
    );
    expect(history.hasPendingLocal).toBe(true);

    await remote(history, stroke(REMOTE, 5, 5, 200), 1);
    expect(red(engine, 5, 5)).toBe(100); // open batch replayed on top

    // Flush and echo everything; the canvas must not change
    const mine = flushStroke(history);
    await remote(history, encodeUndoPoint(LOCAL), 2);
    await remote(history, mine!, 3);
    expect(red(engine, 5, 5)).toBe(100);
    expect(history.hasPendingLocal).toBe(false);
  });

  it("repaints only the region a remote stroke drew", async () => {
    const { engine, history } = setup();
    await remote(history, encodeUndoPoint(REMOTE), 1);
    engine.repaints.length = 0;
    await remote(history, stroke(REMOTE, 5, 5, 100), 2);

    // The engine tracked what the stroke wrote, so the repaint can be that
    // and nothing else. Asking for the whole layer here is the difference
    // between a few hundred pixels and the entire canvas, twice, per message.
    expect(engine.repaints).toEqual(["region:foreground", "region:background"]);
  });

  it("repaints whole layers when a replay rebuilds the canvas", async () => {
    const { engine, history } = setup();
    local(history, encodeUndoPoint(LOCAL));
    localStroke(history, 5, 5, 100);
    engine.repaints.length = 0;

    // Only the same participant can force a rebuild now: their own message
    // arriving from another connection means server order diverged from the
    // fork, which restores a savepoint and puts pixels back the engine never
    // saw written. Another participant's stroke cannot, because it lands in
    // layers this fork does not touch.
    await remote(history, stroke(LOCAL, 5, 5, 200), 1);
    expect(engine.repaints).toContain("all:foreground");
    expect(engine.repaints).toContain("all:background");
  });

  it("does not join a new stroke to the last one when a replay rebuilds the fork", async () => {
    const { engine, history } = setup();
    // A finished gesture, confirmed by the server
    local(history, encodeUndoPoint(LOCAL));
    const first = localStroke(history, 10, 10, 100);
    await remote(history, encodeUndoPoint(LOCAL), 1);
    await remote(history, first, 2);

    // A second gesture, far away, still unconfirmed
    local(history, encodeUndoPoint(LOCAL));
    localStroke(history, 50, 50, 100);

    engine.ops.length = 0;
    // Our own stroke from another connection: server order diverged, so the
    // pending gesture is dropped and history is rebuilt without it.
    await remote(history, stroke(LOCAL, 50, 50, 200), 3);
    expect(engine.ops).toEqual([
      "line:10,10-10,10:100", // confirmed gesture, replayed
      // The same participant, with no undo point since, so this continues
      // their own line rather than opening a new one.
      "line:50,50-10,10:200",
    ]);
    expect(history.hasPendingLocal).toBe(false);

    // The dropped gesture comes back as an ordinary echo, and it opens on its
    // own dot. Continuing from the first gesture's endpoint instead would
    // streak a line from (10,10) across the canvas.
    engine.ops.length = 0;
    await remote(history, encodeUndoPoint(LOCAL), 4);
    await remote(history, stroke(LOCAL, 50, 50, 100), 5);
    expect(engine.ops).toEqual(["line:50,50-50,50:100"]);
  });

  it("treats an actor named as a number and as a string as one person", async () => {
    // Ids reach this history both ways: the wire hands over a 1-byte session
    // id, the public envelope a string. A stroke map that told the two apart
    // would give one person two continuation states, and a gesture would
    // resume from the one its undo point did not clear.
    const { engine, history } = setup();
    history.setLocalUserId(LOCAL); // the number 1
    const draw = (id: string, x: number, y: number) =>
      history.handleLocalOperation({
        id,
        actorId: String(LOCAL), // ...and the string "1"
        operation: {
          kind: "stroke", layer: "foreground", brushSize: 1, brush: "solid",
          color: { r: 100, g: 0, b: 0, a: 255 }, points: [{ x, y }],
          mask: { type: 0, r: 0, g: 0, b: 0 },
        },
      });
    const boundary = (id: string) =>
      history.handleLocalOperation({
        id, actorId: String(LOCAL), operation: { kind: "undo-boundary" },
      });

    boundary("b1");
    draw("s1", 10, 10);
    boundary("b2");
    draw("s2", 50, 50);

    engine.ops.length = 0;
    await remote(history, stroke(LOCAL, 50, 50, 200), 1);

    // The message names user 1; the fork was built from actorId "1". Only if
    // those are one person does this count as our own order diverging, drop
    // the fork and rebuild. Told apart, the fork would survive untouched.
    expect(engine.ops).toEqual(["line:50,50-50,50:200"]);
    expect(history.hasPendingLocal).toBe(false);
  });

  it("drops the fork and converges when the same user draws from another connection", async () => {
    const { engine, history } = setup();
    local(history, encodeUndoPoint(LOCAL));
    const mine = localStroke(history, 3, 3, 100);

    // A different message from the same user arrives first: server order
    // diverged, so the optimistic stroke is rolled back...
    await remote(history, stroke(LOCAL, 4, 4, 50), 1);
    expect(red(engine, 3, 3)).toBe(0);
    expect(red(engine, 4, 4)).toBe(50);
    expect(history.hasPendingLocal).toBe(false);

    // ...and re-appears when its echo arrives in canonical order
    await remote(history, encodeUndoPoint(LOCAL), 2);
    await remote(history, mine, 3);
    expect(red(engine, 3, 3)).toBe(100);
    expect(red(engine, 4, 4)).toBe(50);
  });
});

describe("collaborative undo", () => {
  async function confirmedStroke(
    history: CanvasHistory,
    userId: number,
    x: number,
    y: number,
    r: number,
    seq: number
  ) {
    await remote(history, encodeUndoPoint(userId), seq);
    await remote(history, stroke(userId, x, y, r), seq + 1);
  }

  it("undoes only the sender's stroke and redo restores it", async () => {
    const { engine, history } = setup();
    await confirmedStroke(history, LOCAL, 1, 1, 100, 1);
    await remote(history, stroke(REMOTE, 2, 2, 200), 3);
    expect(history.canUndo()).toBe(true);

    const undo = encodeUndo(LOCAL, false);
    local(history, undo);
    await remote(history, undo);
    expect(red(engine, 1, 1)).toBe(0); // local stroke reverted
    expect(redFor(engine, REMOTE, 2, 2)).toBe(200); // remote stroke preserved
    expect(history.canUndo()).toBe(false);
    expect(history.canRedo()).toBe(true);

    const redo = encodeUndo(LOCAL, true);
    local(history, redo);
    await remote(history, redo);
    expect(red(engine, 1, 1)).toBe(100);
    expect(history.canUndo()).toBe(true);
    expect(history.canRedo()).toBe(false);
  });

  it("reflects a fill's undo to the participants watching it", async () => {
    const { engine, history } = setup();
    // Someone else fills and then undoes it. We hold no fork of our own, so
    // this is purely the receiving side: mark and replay.
    await remote(history, encodeUndoPoint(REMOTE), 1);
    await remote(
      history,
      encodeFill(REMOTE, REMOTE, "foreground", 4, 4, 30, 0, 0, 255),
      2
    );
    expect(redFor(engine, REMOTE, 1, 1)).toBe(30);

    await remote(history, encodeUndo(REMOTE, false), 3);
    expect(redFor(engine, REMOTE, 1, 1)).toBe(0);
  });

  it("reflects our own fill's undo, once the server echoes it", async () => {
    const { engine, history } = setup();
    local(history, encodeUndoPoint(LOCAL));
    const bytes = encodeFill(LOCAL, LOCAL, "foreground", 4, 4, 30, 0, 0, 255);
    local(history, bytes);
    expect(red(engine, 1, 1)).toBe(30);

    await remote(history, encodeUndoPoint(LOCAL), 1);
    await remote(history, bytes, 2);
    const undo = encodeUndo(LOCAL, false);
    local(history, undo);
    await remote(history, undo, 3);
    expect(red(engine, 1, 1)).toBe(0);
  });

  it("repaints the canvas of whoever's fill was undone, not only our own", async () => {
    // A replay rewrites the layers of whichever participants the entries
    // touch. Repainting only ours leaves theirs showing pixels their buffer
    // no longer has -- and a fill covers the whole layer, so what stays on
    // screen is the entire fill.
    const { engine, history } = setup();
    await remote(history, encodeUndoPoint(REMOTE), 1);
    await remote(
      history,
      encodeFill(REMOTE, REMOTE, "foreground", 4, 4, 30, 0, 0, 255),
      2
    );

    engine.ownedRepaints.length = 0;
    await remote(history, encodeUndo(REMOTE, false), 3);

    expect(redFor(engine, REMOTE, 1, 1)).toBe(0);
    expect(engine.ownedRepaints).toContain(`all:${REMOTE}/foreground`);
  });

  it("a new stroke kills the redo stack", async () => {
    const { history } = setup();
    await confirmedStroke(history, LOCAL, 1, 1, 100, 1);
    const undo = encodeUndo(LOCAL, false);
    local(history, undo);
    await remote(history, undo);
    expect(history.canRedo()).toBe(true);

    await confirmedStroke(history, LOCAL, 2, 2, 110, 3);
    expect(history.canRedo()).toBe(false);
    expect(history.canUndo()).toBe(true);
  });

  it("another user's undo does not affect local undo state", async () => {
    const { engine, history } = setup();
    await confirmedStroke(history, LOCAL, 1, 1, 100, 1);
    await confirmedStroke(history, REMOTE, 2, 2, 200, 3);

    await remote(history, encodeUndo(REMOTE, false));
    expect(red(engine, 2, 2)).toBe(0);
    expect(red(engine, 1, 1)).toBe(100);
    expect(history.canUndo()).toBe(true);
    expect(history.canRedo()).toBe(false);
  });

  it("disables undo immediately while an undo is in flight", async () => {
    const { history } = setup();
    await remote(history, encodeUndoPoint(LOCAL), 1);
    await remote(history, stroke(LOCAL, 1, 1, 100), 2);
    expect(history.canUndo()).toBe(true);

    // Click undo: not yet echoed, but a second click must be refused
    local(history, encodeUndo(LOCAL, false));
    expect(history.canUndo()).toBe(false);
    expect(history.canRedo()).toBe(true);

    await remote(history, encodeUndo(LOCAL, false));
    expect(history.canUndo()).toBe(false);
    expect(history.canRedo()).toBe(true);
  });

  it("undoes a fill", async () => {
    const { engine, history } = setup();
    await remote(history, encodeUndoPoint(LOCAL), 1);
    await remote(
      history,
      encodeFill(LOCAL, LOCAL, "foreground", 0, 0, 100, 0, 0, 255),
      2
    );
    expect(red(engine, 8, 8)).toBe(100);

    await remote(history, encodeUndo(LOCAL, false));
    expect(red(engine, 8, 8)).toBe(0);
  });
});

describe("reset point squashing", () => {
  it("freezes undo below the base seq but keeps newer strokes undoable", async () => {
    const { engine, history } = setup();
    await remote(history, encodeUndoPoint(LOCAL), 1);
    await remote(history, stroke(LOCAL, 1, 1, 10), 2);
    await remote(history, encodeUndoPoint(LOCAL), 3);
    await remote(history, stroke(LOCAL, 2, 2, 20), 4);

    await history.handleResetPoint(2);

    // The first stroke is squashed into the base savepoint: still visible,
    // no longer undoable
    expect(red(engine, 1, 1)).toBe(10);
    expect(history.canUndo()).toBe(true); // second stroke only

    const undo = encodeUndo(LOCAL, false);
    local(history, undo);
    await remote(history, undo);
    expect(red(engine, 2, 2)).toBe(0);
    expect(red(engine, 1, 1)).toBe(10); // squashed stroke untouched
    expect(history.canUndo()).toBe(false);
  });
});

describe("reconnect", () => {
  it("replays history onto a blank canvas instead of the stale drawing", async () => {
    const { engine, history } = setup();
    await remote(history, encodeUndoPoint(REMOTE), 1);
    await remote(history, stroke(REMOTE, 1, 1, 50), 2);
    await remote(history, encodeFill(REMOTE, REMOTE, "foreground", 4, 4, 30, 0, 0, 255), 3);
    expect(redFor(engine, REMOTE, 1, 1)).toBe(30);
    const opsBeforeDrop = [...engine.ops];

    // The connection drops and comes back: the canvas still shows everything
    // above, and the server is about to replay all of it from seq 1
    history.resetToBlankCanvas();
    expect(redFor(engine, REMOTE, 1, 1)).toBe(0);
    expect(redFor(engine, REMOTE, 4, 4)).toBe(0);

    engine.ops.length = 0;
    await remote(history, encodeUndoPoint(REMOTE), 1);
    await remote(history, stroke(REMOTE, 1, 1, 50), 2);
    await remote(history, encodeFill(REMOTE, REMOTE, "foreground", 4, 4, 30, 0, 0, 255), 3);

    // The replay reproduces the drawing exactly, doing the same work once
    expect(redFor(engine, REMOTE, 1, 1)).toBe(30);
    expect(engine.ops).toEqual(opsBeforeDrop);
  });

  it("keeps a reconnecting user's own strokes undoable under their new id", async () => {
    const { engine, history } = setup();
    await remote(history, encodeUndoPoint(LOCAL), 1);
    await remote(history, stroke(LOCAL, 2, 2, 40), 2);

    history.resetToBlankCanvas();
    // The server hands back the same session user id, since the id map
    // outlives the connection that owned it
    history.setLocalUserId(LOCAL);
    await remote(history, encodeUndoPoint(LOCAL), 1);
    await remote(history, stroke(LOCAL, 2, 2, 40), 2);

    expect(red(engine, 2, 2)).toBe(40);
    const undo = encodeUndo(LOCAL, false);
    local(history, undo);
    await remote(history, undo);
    expect(red(engine, 2, 2)).toBe(0);
  });
});

describe("brush type wire codes", () => {
  it("round trips every drawing tool", () => {
    for (const brushType of WIRE_BRUSH_TYPES) {
      const bytes = encodeStroke(REMOTE, REMOTE, "foreground", 4, brushType, 1, 2, 3, 255, [
        { x: 5, y: 6 },
      ]);
      const msg = decodeMessage(bytes);
      expect(msg?.type).toBe("stroke");
      expect(msg && msg.type === "stroke" && msg.brushType).toBe(brushType);
    }
  });

  it("assigns codes that never move", () => {
    // A client that predates a code cannot learn it, so renumbering these
    // would silently redraw the history of any session it shares.
    expect(BRUSH_TYPE).toEqual({
      SOLID: 0,
      HALFTONE: 1,
      ERASER: 2,
      BRUSH: 3,
      DODGE: 4,
      BURN: 5,
      BLUR: 6,
    });
  });

  it("falls back to solid, not eraser, on a code it does not know", () => {
    // Forward compatibility: meeting a tool from a newer client should draw
    // something harmless rather than delete what is underneath.
    const bytes = encodeStroke(REMOTE, REMOTE, "foreground", 4, "solid", 1, 2, 3, 255, [
      { x: 5, y: 6 },
    ]);
    new Uint8Array(bytes)[4] = 99;
    const msg = decodeMessage(bytes);
    expect(msg && msg.type === "stroke" && msg.brushType).toBe("solid");
  });
});


describe("the tool messages through history", () => {
  it("pastes what the sender copied, not what the receiver did", async () => {
    const { engine, history } = setup();

    // Two users copy different pixels, each out of their own layers.
    engine.layersFor(String(LOCAL)).foreground[(1 * SIZE + 1) * 4] = 111;
    engine.layersFor(String(REMOTE)).foreground[(2 * SIZE + 2) * 4] = 222;

    const copy = (user: number, x: number, y: number) =>
      encodeRegion(user, user, "foreground", "copy",
        { x, y, width: 1, height: 1 }, { r: 0, g: 0, b: 0, a: 255 }, 1);

    await remote(history, copy(LOCAL, 1, 1), 1);
    await remote(history, copy(REMOTE, 2, 2), 2);

    // LOCAL copied last on this client, but it is REMOTE that pastes
    await remote(history, encodeRegion(REMOTE, REMOTE, "foreground", "paste",
      { x: 8, y: 8, width: 1, height: 1 }, { r: 0, g: 0, b: 0, a: 255 }, 1), 3);
    expect(redFor(engine, REMOTE, 8, 8)).toBe(222);

    // and LOCAL's own paste still gets LOCAL's clipboard
    await remote(history, encodeRegion(LOCAL, LOCAL, "foreground", "paste",
      { x: 9, y: 9, width: 1, height: 1 }, { r: 0, g: 0, b: 0, a: 255 }, 1), 4);
    expect(red(engine, 9, 9)).toBe(111);
  });

  it("treats every tool message as undoable history", async () => {
    const { engine, history } = setup();
    const color = { r: 42, g: 0, b: 0, a: 255 };

    await remote(history, encodeUndoPoint(LOCAL), 1);
    await remote(history, encodeRegion(LOCAL, LOCAL, "foreground", "rectFill",
      { x: 3, y: 3, width: 2, height: 2 }, color, 1), 2);
    expect(red(engine, 3, 3)).toBe(42);
    expect(history.canUndo()).toBe(true);

    // Undo only lands once the server echoes it back
    const undo = encodeUndo(LOCAL, false);
    local(history, undo);
    await remote(history, undo, 3);
    expect(red(engine, 3, 3)).toBe(0);
  });

  it("replays a remote bezier, line, eraseAll and text", async () => {
    const { engine, history } = setup();
    const color = { r: 77, g: 0, b: 0, a: 255 };

    await remote(history, encodeBezier(REMOTE, REMOTE, "foreground", 2, "solid", color,
      [4, 5, 0, 0, 0, 0, 9, 9]), 1);
    expect(redFor(engine, REMOTE, 4, 5)).toBe(77);

    await remote(history, encodeText(REMOTE, REMOTE, "foreground", 6, 6, "hi", color, 4), 2);
    expect(redFor(engine, REMOTE, 6, 6)).toBe(77);

    await remote(history, encodeLine(REMOTE, REMOTE, "foreground", 1, "solid", color,
      { x: 1, y: 1 }, { x: 2, y: 2 }), 3);

    await remote(history, encodeEraseAll(REMOTE, REMOTE, "foreground"), 4);
    expect(redFor(engine, REMOTE, 4, 5)).toBe(0);
    expect(engine.ops).toContain("eraseAll:foreground");
  });
});

/**
 * A mask changes which pixels a stroke touches, so it has to reach every
 * client that replays the stroke. These drive the real path: a local stroke
 * is encoded exactly as it would be sent, then handed to a second history as
 * if it had arrived over the wire.
 */
describe("masks over the wire", () => {
  const MASK = { type: 2, r: 18, g: 52, b: 86 };

  it("applies the sender's mask on the receiving client", async () => {
    const { history } = setup();
    history.addLocalSegment(
      "foreground", 1, "solid", { r: 50, g: 0, b: 0, a: 255 }, 1, 1, MASK
    );
    const bytes = flushStroke(history);

    // A second client, which has only the bytes
    const receiver = setup();
    await remote(receiver.history, bytes, 1);

    expect(receiver.engine.ops).toEqual(["line:1,1-1,1:50:mask2(18,52,86)"]);
  });

  it("leaves the engine unmasked for a stroke drawn without one", async () => {
    const receiver = setup();
    // A masked stroke first, so the engine is left holding a mask
    const masked = setup();
    masked.history.addLocalSegment(
      "foreground", 1, "solid", { r: 50, g: 0, b: 0, a: 255 }, 1, 1, MASK
    );
    await remote(receiver.history, flushStroke(masked.history), 1);

    // Then an unmasked one from someone else must not inherit it
    const plain = setup();
    plain.history.addLocalSegment(
      "foreground", 1, "solid", { r: 60, g: 0, b: 0, a: 255 }, 2, 2
    );
    await remote(receiver.history, flushStroke(plain.history), 2);

    // Both came from the same session id, so the second continues the first's
    // stroke rather than starting a dot -- what matters here is the mask.
    expect(receiver.engine.ops[1]).toContain(":60");
    expect(receiver.engine.ops[1]).not.toContain("mask");
  });

  it("keeps the mask when the stroke is replayed from history", async () => {
    const { engine, history } = setup();
    const sender = setup();
    sender.history.addLocalSegment(
      "foreground", 1, "solid", { r: 50, g: 0, b: 0, a: 255 }, 1, 1, MASK
    );
    const bytes = flushStroke(sender.history);

    await remote(history, encodeUndoPoint(REMOTE), 1);
    await remote(history, bytes, 2);
    const drawn = engine.ops.length;

    // An undo forces a rebuild from the last savepoint, replaying the stroke
    await remote(history, encodeUndo(REMOTE, false), 3);
    await remote(history, encodeUndo(REMOTE, true), 4);

    const replayed = engine.ops.slice(drawn).filter((op) => op.startsWith("line:"));
    for (const op of replayed) expect(op).toContain("mask2(18,52,86)");
  });
});

describe("undoing a mark made on somebody else's layers", () => {
  it("undoes that mark only, leaving our own layers where they were", async () => {
    const { engine, history } = setup();

    // 1. We fill our own background.
    await confirmedFill(history, LOCAL, LOCAL, 4, 4, 30, 1);
    expect(engine.layersFor(String(LOCAL)).background[0]).toBe(30);

    // 2. We clear it again.
    await remote(history, encodeUndoPoint(LOCAL), 3);
    await remote(history, encodeEraseAll(LOCAL, LOCAL, "background"), 4);
    expect(engine.layersFor(String(LOCAL)).background[0]).toBe(0);

    // 3. We draw on somebody else's layers.
    await remote(history, encodeUndoPoint(LOCAL), 5);
    await remote(
      history,
      encodeStroke(LOCAL, REMOTE, "foreground", 1, "solid", 90, 0, 0, 255, [
        { x: 2, y: 2 },
      ]),
      6
    );
    expect(redFor(engine, REMOTE, 2, 2)).toBe(90);

    // 4. Undo. Only step 3 goes.
    await remote(history, encodeUndo(LOCAL, false), 7);
    expect(redFor(engine, REMOTE, 2, 2)).toBe(0);
    // Our own background stays cleared: step 2 is not undone with it.
    expect(engine.layersFor(String(LOCAL)).background[0]).toBe(0);
  });
});

describe("undo through the public operation path", () => {
  it("keeps a clear that is older than the undone mark", async () => {
    const { engine, history } = setup();
    history.setLocalUserId("1");
    const op = (sequence: number, operation: unknown, id = `a:${sequence}`) =>
      history.handleCanonicalOperation({
        id, actorId: "1", sequence,
        operation: operation as never,
      });

    await op(1, { kind: "undo-boundary" });
    await op(2, {
      kind: "fill", layer: "background", targetActorId: "1",
      at: { x: 1, y: 1 }, color: { r: 200, g: 0, b: 0, a: 255 },
      mask: { type: 0, r: 0, g: 0, b: 0 },
    });
    expect(engine.layersFor("1").background[0]).toBe(200);

    await op(3, { kind: "undo-boundary" });
    await op(4, { kind: "clear-layer", layer: "background", targetActorId: "1" });
    expect(engine.layersFor("1").background[0]).toBe(0);

    await op(5, { kind: "undo-boundary" });
    await op(6, {
      kind: "stroke", layer: "foreground", targetActorId: "2",
      brushSize: 1, brush: "solid", color: { r: 90, g: 0, b: 0, a: 255 },
      points: [{ x: 2, y: 2 }], mask: { type: 0, r: 0, g: 0, b: 0 },
    });
    await op(7, { kind: "undo", redo: false });

    expect(redFor(engine, 2, 2, 2)).toBe(0);
    expect(engine.layersFor("1").background[0]).toBe(0);
  });
});

describe("savepoints and the marks the canvas already carries", () => {
  it("does not lose optimistic marks when a savepoint is taken over them", async () => {
    // The controlled path paints through the interactive canvas and only
    // registers the operation here. Those pixels are real and on screen, so a
    // savepoint that shares the previous one's arrays for that participant is
    // out of date the moment it is taken -- and an undo, which restores a
    // savepoint, loses exactly the marks made since. Locally only: everyone
    // else applied them through the source that marks.
    const { engine, history } = setup();
    history.setLocalUserId("1");

    const paint = (owner: string, x: number, y: number, r: number) => {
      // What the interactive canvas does: draw through the engine itself...
      engine.drawLine(
        engine.layersFor(owner).foreground,
        x, y, x, y, 1, "solid", r,
      );
      // ...and tell the history the operation happened.
      history.registerOptimisticOperation({
        id: `local:${owner}:${x}`,
        actorId: "1",
        operation: {
          kind: "stroke", layer: "foreground", targetActorId: owner,
          brushSize: 1, brush: "solid",
          color: { r, g: 0, b: 0, a: 255 },
          points: [{ x, y }], mask: { type: 0, r: 0, g: 0, b: 0 },
        },
      });
    };

    history.registerOptimisticOperation({
      id: "local:b", actorId: "1", operation: { kind: "undo-boundary" },
    });
    paint("1", 3, 3, 77);
    paint("2", 5, 5, 88);

    // The server confirms all of it. The echo carries the same id the fork
    // entry has -- that is how a client recognises its own work coming back --
    // so it takes the path that neither draws (the pixels are already there)
    // nor records that anything changed.
    for (const id of ["local:b", "local:1:3", "local:2:5"]) {
      await history.handleCanonicalOperation({
        id,
        actorId: "1",
        sequence: 1 + ["local:b", "local:1:3", "local:2:5"].indexOf(id),
        operation:
          id === "local:b"
            ? { kind: "undo-boundary" }
            : {
                kind: "stroke",
                layer: "foreground",
                targetActorId: id === "local:1:3" ? "1" : "2",
                brushSize: 1,
                brush: "solid",
                color: { r: id === "local:1:3" ? 77 : 88, g: 0, b: 0, a: 255 },
                points: [{ x: id === "local:1:3" ? 3 : 5, y: id === "local:1:3" ? 3 : 5 }],
                mask: { type: 0, r: 0, g: 0, b: 0 },
              },
      });
    }
    history.takeSavepointForTest();

    // A later gesture, and an undo of just that one.
    await remote(history, encodeUndoPoint(LOCAL), 4);
    await remote(
      history,
      encodeFill(LOCAL, 2, "background", 1, 1, 99, 0, 0, 255),
      5
    );
    await remote(history, encodeUndo(LOCAL, false), 6);

    // The fill is gone and the earlier marks are still there.
    expect(engine.layersFor("2").background[0]).toBe(0);
    expect(redFor(engine, 1, 3, 3)).toBe(77);
    expect(redFor(engine, 2, 5, 5)).toBe(88);
  });
});

describe("savepoint memory", () => {
  /**
   * Savepoints are bounded by what they weigh, not only by how many there are.
   *
   * The count says nothing about the cost: one savepoint holds a layer pair per
   * participant, so eight of them in a full room on a large canvas is most of
   * half a gigabyte. The budget is what keeps that from being unbounded in the
   * dimension that actually runs out.
   */
  it("drops old savepoints once they weigh more than the budget allows", () => {
    const engine = new FakeEngine();
    const layerBytes = SIZE * SIZE * 4;
    // Room for two savepoints' worth of one participant's pair, and no more.
    const budget = layerBytes * 4;
    const history = new CanvasHistory(
      engine as unknown as DrawingEngine,
      undefined,
      { maxSavepointBytes: budget },
    );
    history.setLocalUserId(LOCAL);

    for (let round = 0; round < 8; round++) {
      // A write between savepoints, or the next one shares these arrays and
      // costs nothing -- which is the case the budget must *not* punish.
      engine.noteWrite(String(LOCAL));
      history.takeSavepointForTest();
      expect(history.savepointBytesForTest()).toBeLessThanOrEqual(budget);
    }

    // The base and the newest are always kept: every rollback starts from one
    // of the two, so evicting down to nothing would trade memory for a replay
    // that cannot happen.
    expect(history.savepointCountForTest()).toBeGreaterThanOrEqual(2);
  });

  it("keeps a full set when the participants have not drawn between them", () => {
    const engine = new FakeEngine();
    const history = new CanvasHistory(engine as unknown as DrawingEngine);
    history.setLocalUserId(LOCAL);

    // No writes, so every savepoint shares the previous one's arrays. Weighing
    // them naively would count the same buffer over and over and evict
    // savepoints that cost nothing to hold.
    for (let round = 0; round < 8; round++) history.takeSavepointForTest();

    expect(history.savepointBytesForTest()).toBe(SIZE * SIZE * 4 * 2);
  });
});

describe("the pen the local user is holding", () => {
  /**
   * A controlled painter draws straight into the engine between the messages
   * this history applies, so whatever an applied operation leaves in the
   * engine's pen, the local stroke's next segment is drawn with. It used to
   * leave the remote author's continuation state there, so the local stroke
   * joined onto somebody else's endpoint here and onto its own everywhere
   * else -- a doubled or missing joint pixel only its author could see.
   */
  it("is handed back as it was after somebody else's operations", async () => {
    const { engine, history } = setup();
    const copied = { pasted: 42 } as unknown as ImageData;
    engine.setStrokeState([[5, 5], [4, 4]]);
    engine.maskType = 2;
    engine.maskColor = [1, 2, 3];
    engine.setClipboard(copied);

    await remote(history, encodeUndoPoint(REMOTE), 1);
    await remote(history, stroke(REMOTE, 10, 10, 200), 2);
    await remote(
      history,
      encodeLine(REMOTE, REMOTE, "foreground", 1, "solid", { r: 9, g: 0, b: 0, a: 255 }, { x: 1, y: 1 }, { x: 3, y: 3 }),
      3,
    );
    await remote(
      history,
      encodeRegion(REMOTE, REMOTE, "foreground", "copy", { x: 0, y: 0, width: 2, height: 2 }, { r: 0, g: 0, b: 0, a: 255 }, 1),
      4,
    );

    expect(engine.getStrokeState()).toEqual([[5, 5], [4, 4]]);
    expect(engine.maskType).toBe(2);
    expect(engine.maskColor).toEqual([1, 2, 3]);
    expect(engine.getClipboard()).toBe(copied);
  });

  /**
   * A copy before a savepoint and its paste after it: the replay has to paste
   * what was copied *there*, not whatever its author copied last.
   */
  it("replays a paste with the copy that was current at its position", async () => {
    const { engine, history } = setup();
    const region = (tool: "copy" | "paste", x: number, y: number) =>
      encodeRegion(REMOTE, REMOTE, "foreground", tool, { x, y, width: 1, height: 1 }, { r: 0, g: 0, b: 0, a: 255 }, 1);
    await remote(history, stroke(REMOTE, 1, 1, 50), 1);
    await remote(history, region("copy", 1, 1), 2);
    history.takeSavepointForTest();
    await remote(history, region("paste", 5, 5), 3);
    await remote(history, stroke(REMOTE, 1, 1, 90), 4);
    await remote(history, region("copy", 1, 1), 5);
    expect(redFor(engine, REMOTE, 5, 5)).toBe(50);

    // A third person's undo rebuilds everything after the savepoint.
    await remote(history, encodeUndoPoint(3), 6);
    await remote(history, stroke(3, 9, 9, 10), 7);
    await remote(history, encodeUndo(3, false), 8);

    expect(redFor(engine, REMOTE, 5, 5)).toBe(50);
  });
});

describe("the stroke under the pointer", () => {
  const LOCAL_POINT = { x: 3, y: 3 };
  const inProgress = (history: CanvasHistory, engine: FakeEngine) => {
    let drawing = true;
    let flushed = 0;
    let counter = 0;
    // Like the painter's own: a flush with nothing new since the last one
    // sends nothing.
    let unsent = true;
    history.setLocalWork({
      drawing: () => drawing,
      flush: () => {
        if (!unsent) return;
        unsent = false;
        flushed += 1;
        counter += 1;
        history.registerOptimisticOperation({
          id: `local:${counter}`,
          actorId: String(LOCAL),
          operation: {
            kind: "stroke",
            layer: "foreground",
            brushSize: 1,
            brush: "solid",
            color: { r: 150, g: 0, b: 0, a: 255 },
            points: [LOCAL_POINT],
            mask: { type: 0, r: 0, g: 0, b: 0 },
          },
        });
      },
    });
    // What the pointer has painted and not yet handed over.
    engine.layers.foreground[(LOCAL_POINT.y * SIZE + LOCAL_POINT.x) * 4] = 150;
    return {
      flushed: () => flushed,
      lift: () => {
        drawing = false;
      },
    };
  };

  it("survives a replay that happens before it is handed over", async () => {
    const { engine, history } = setup();
    inProgress(history, engine);
    await remote(history, encodeUndoPoint(REMOTE), 1);
    await remote(history, stroke(REMOTE, 10, 10, 200), 2);
    await remote(history, encodeUndo(REMOTE, false), 3);

    expect(redFor(engine, REMOTE, 10, 10)).toBe(0);
    expect(red(engine, LOCAL_POINT.x, LOCAL_POINT.y)).toBe(150);
  });

  it("is handed over before somebody marks the layers it is on, and only then", async () => {
    const { engine, history } = setup();
    const pen = inProgress(history, engine);
    await remote(history, stroke(REMOTE, 10, 10, 200), 1);
    expect(pen.flushed()).toBe(0);

    await remote(
      history,
      encodeStroke(REMOTE, LOCAL, "foreground", 1, "solid", 90, 0, 0, 255, [LOCAL_POINT]),
      2,
    );
    expect(pen.flushed()).toBe(1);
  });

  it("keeps savepoints from being taken until it is finished", async () => {
    const { engine, history } = setup();
    const pen = inProgress(history, engine);
    for (let seq = 1; seq <= 70; seq++) {
      await remote(history, stroke(REMOTE, seq % SIZE, 0, seq), seq);
    }
    expect(history.savepointCountForTest()).toBe(1);

    pen.lift();
    await remote(history, stroke(REMOTE, 1, 1, 1), 71);
    expect(history.savepointCountForTest()).toBe(2);
  });
});
