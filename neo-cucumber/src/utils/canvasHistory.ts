/*
 * Derived from Drawpile's `canvas_history.c`:
 *   Copyright (C) 2022 askmeaboutloom
 *   GNU General Public License, version 3 or (at your option) any later
 *   version. See THIRD-PARTY-NOTICES.md at the root of this repository.
 *
 * The entry list, the savepoint scheme, the optimistic fork and the
 * concurrency check that decides between applying a remote message directly
 * and replaying history under it all follow that file. This is the one place
 * in this repository that does; everywhere else Drawpile is named, it is a
 * design being referred to rather than code being carried over.
 *
 * oeee-cafe is distributed under the GNU Affero General Public License v3,
 * which AGPL-3 section 13 permits to be combined with GPL-3 code.
 */

/**
 * Client-side canonical canvas history for the shared-layer collaboration
 * model.
 *
 * All clients receive the same server-sequenced message stream and fold it
 * into this history, so marking strokes undone and replaying from a savepoint
 * is deterministic across every participant:
 *
 * - Confirmed messages append to `entries`; periodic savepoints (layer copies
 *   plus per-user stroke state) bound the cost of replays.
 * - Local messages are applied optimistically and queued in the fork until
 *   the server echoes them back in canonical order. A remote message that
 *   doesn't touch any fork entry's affected area applies directly; one that
 *   overlaps triggers a replay of history with the fork re-applied on top.
 * - UNDO_POINT messages delimit strokes; UNDO/redo marks a user's entries and
 *   replays. Undo takes effect on server echo (like Drawpile), keeping every
 *   client's marking order identical.
 * - RESET_POINT (sent after a server session reset) squashes everything at or
 *   below the reset's base sequence into a new base savepoint.
 */

import { DrawingEngine } from "../DrawingEngine";
import { NO_MASK, type Mask } from "../neo/mask";
import type {
  HistoryActorId,
  HistoryOperation,
  HistoryFillRegion,
  HistorySnapshot,
  HistoryStroke,
} from "../synchronization/historyOperations";
import {
  fromHistoryOperation,
  toHistoryOperation,
} from "../synchronization/historyOperations";
import type {
  CanonicalPainterOperation,
  LocalPainterOperation,
} from "../operations";
import { cloneOperation } from "./cloneOperation";
import { pngDataToLayer } from "./canvasSnapshot";
import { inflateCoverage } from "./rasterCodec";
import { extentFor, fontSizeForBrush, TEXT_FONT_FAMILY } from "../neo/tools";
import type { WireBrushType } from "../types/drawing";

type LayerName = "foreground" | "background";
type StrokeState = [[number, number], [number, number]] | null;
/**
 * Per-actor state, keyed by `actorKey`. An actor arrives as a 1-byte session
 * id from the wire and as a string through the public envelope, and a Map
 * tells `1` and `"1"` apart -- so one person would hold two entries and their
 * next stroke would continue from the other one's endpoint, drawing a line
 * across the canvas. Every lookup goes through `actorKey` to prevent that.
 */
type ActorKey = string;
/**
 * What each actor's next operation depends on besides the layers: where their
 * pen left off, and what they last copied.
 *
 * Both are part of a position in history, not of the client replaying it. A
 * replay from a savepoint has to paste the copy that was current *there*, and
 * a pass that runs history into scratch buffers -- compaction -- must not
 * leave its older copy behind as the live one. So the clipboards travel with
 * the continuation states, cloned and restored with them, rather than living
 * in one map that every pass writes into.
 */
class StrokeStates extends Map<ActorKey, StrokeState> {
  readonly clipboards = new Map<ActorKey, ImageData | null>();
}

const actorKey = (id: HistoryActorId): ActorKey => String(id);

type UndoState = "done" | "undone" | "gone";

interface Entry {
  seq?: number;
  msg: HistoryOperation;
  undo: UndoState;
}

/** One participant's two buffers, as stored in a savepoint. */
interface OwnerLayers {
  foreground: Uint8ClampedArray;
  background: Uint8ClampedArray;
}

interface Savepoint {
  index: number; // number of history entries applied in this state
  /**
   * Every participant's pair at this point in history.
   *
   * A participant whose layers have not changed since the previous savepoint
   * shares that savepoint's arrays rather than a copy of them: with eight
   * participants and eight savepoints, copying all of them every time would
   * cost a few hundred megabytes at the largest canvas size, and a savepoint
   * is only ever read, never drawn into, so sharing is safe.
   */
  layers: Map<ActorKey, OwnerLayers>;
  /**
   * What the engine's write count stood at for each participant when this was
   * taken, so the next savepoint can ask whether their layers have moved on
   * rather than trusting whoever wrote to them to have said so.
   */
  generations: Map<ActorKey, number>;
  strokes: StrokeStates;
}

/** Where an operation's pixels live: one participant's pair, by author. */
type LayerSource = (owner: ActorKey) => OwnerLayers;

type Area =
  | {
      kind: "pixels";
      /** Whose layer pair the rectangle is in. */
      owner: ActorKey;
      layer: LayerName;
      x0: number;
      y0: number;
      x1: number;
      y1: number;
    }
  | { kind: "user" }
  | { kind: "everything" };

interface ForkEntry {
  id: string;
  msg: HistoryOperation;
  area: Area;
}

// A stroke being drawn locally: its segments are painted optimistically as
// they happen, then flushed into one STROKE message (a fork entry) at a time
interface OpenBatch {
  /** Whose pair the segments land in; the local participant's by default. */
  targetOwner: ActorKey;
  layer: LayerName;
  brushSize: number;
  brushType: WireBrushType;
  color: { r: number; g: number; b: number; a: number };
  mask: Mask;
  points: { x: number; y: number }[];
  area: {
    kind: "pixels";
    owner: ActorKey;
    layer: LayerName;
    x0: number;
    y0: number;
    x1: number;
    y1: number;
  };
}

const SAVEPOINT_INTERVAL = 64;
const MAX_SAVEPOINTS = 8;

/**
 * How much a session's savepoints may weigh between them.
 *
 * A savepoint holds every participant's layer pair, so what eight of them cost
 * depends on who is in the room and how big the canvas is -- at the largest
 * session on the largest canvas, a count of eight is most of half a gigabyte
 * of a tab that also has to draw. A count alone cannot bound that, because the
 * count says nothing about what a copy weighs.
 *
 * So they are bounded by both. Dropping one costs nothing but a longer replay
 * on the paths that reach for an older position -- a diverged fork, a
 * checkpoint's cut -- and never the common one, which always starts from the
 * newest savepoint. Trading replay length for memory is the right way round
 * when memory is what has run out.
 */
const MAX_SAVEPOINT_BYTES = 64 * 1024 * 1024;

/**
 * How far the optimistic fork may fall behind canonical order before it is
 * given up on.
 *
 * Drawpile's `MAX_FALLBEHIND`. Every canonical message that arrives while our
 * own are still unconfirmed counts once, and the count resets the moment the
 * fork empties. Past this the echoes are evidently not coming, and holding
 * optimistic pixels for them means showing a canvas nobody else has -- so the
 * fork is dropped and history rebuilt from what the server actually said. Any
 * message that was merely late comes back as an ordinary echo.
 *
 * Drawpile also declines to drop the fork mid-stroke, to keep a rollback from
 * pulling ink out from under the pen and the next segment rebuilding a fork to
 * drop again. Its non-concurrent rollback needs that guard because it clears
 * the fork by default; the equivalent branch here never clears it -- a replay
 * re-applies the pending work on top -- so only this bound was missing.
 */
export const MAX_FORK_FALLBEHIND = 10000;

/** How many recent decisions the synchronisation trace keeps. */
const TRACE_LENGTH = 512;

/**
 * One thing that happened to this history, and what was decided about it.
 *
 * Drawpile records every input to its canvas history -- local message, remote
 * message, reset, cleanup -- so a divergence somebody reports can be replayed
 * offline against the exact sequence that produced it (`DP_DUMP_*` in
 * `canvas_history.h`). A replay that renders differently from the canvas it
 * was recorded on is the worst failure this codebase can produce, and until
 * now the only evidence of one was that somebody noticed.
 *
 * This is the same idea at the granularity that matters here: not the pixels,
 * which are large and reproducible from the operations, but the order things
 * arrived in and which branch of reconciliation each one took.
 */
export interface HistoryTraceEvent {
  /** Milliseconds since this history was constructed. */
  at: number;
  source: "local" | "canonical" | "reset";
  /** The operation type, e.g. `stroke`, `undo`, `fillRegion`. */
  op: string;
  actor: string;
  seq?: number;
  /**
   * Which way reconciliation went, for canonical messages:
   *
   * - `echo` -- matched the head of our own fork, already on the canvas
   * - `concurrent` -- someone else's, touching none of our pending pixels
   * - `replay` -- overlapped pending work, so history was rebuilt under it
   * - `abandoned` -- matched our fork further down; the entries before it
   *   never reached the server and were dropped
   * - `diverged` -- our own id from elsewhere; the fork was dropped
   * - `fallbehind` -- the fork waited too long and was given up on
   * - `applied` -- nothing pending, applied straight
   */
  action?:
    | "echo"
    | "concurrent"
    | "replay"
    | "abandoned"
    | "diverged"
    | "fallbehind"
    | "applied";
}

function transportId(bytes: Uint8Array): string {
  let result = "";
  for (const byte of bytes) result += byte.toString(16).padStart(2, "0");
  return result;
}

function cloneStrokes(strokes: StrokeStates): StrokeStates {
  const copy = new StrokeStates();
  // Shared rather than copied: a copy makes a new ImageData and nothing
  // writes into one after, so two positions can hold the same image.
  for (const [userId, clipboard] of strokes.clipboards) {
    copy.clipboards.set(userId, clipboard);
  }
  for (const [userId, state] of strokes) {
    copy.set(
      userId,
      state === null
        ? null
        : [
            [state[0][0], state[0][1]],
            [state[1][0], state[1][1]],
          ]
    );
  }
  return copy;
}

function affectedArea(msg: HistoryOperation): Area {
  // The rectangle is in the layers the operation *writes*, which is usually
  // its author's own pair but need not be: a participant may paint into
  // somebody else's. Naming the author here instead would call two people
  // drawing on one layer concurrent and let them diverge.
  const owner = "targetOwner" in msg ? actorKey(msg.targetOwner) : "";
  switch (msg.type) {
    case "stroke": {
      const pad = msg.brushSize;
      let x0 = Infinity;
      let y0 = Infinity;
      let x1 = -Infinity;
      let y1 = -Infinity;
      for (const p of msg.points) {
        x0 = Math.min(x0, p.x);
        y0 = Math.min(y0, p.y);
        x1 = Math.max(x1, p.x);
        y1 = Math.max(y1, p.y);
      }
      return {
        kind: "pixels", owner,
        layer: msg.layer,
        x0: x0 - pad,
        y0: y0 - pad,
        x1: x1 + pad,
        y1: y1 + pad,
      };
    }
    case "region": {
      // The tool table already knows what each op reads and writes -- turn
      // squares off the rectangle, blur reaches a pixel past it, merge takes
      // both layers. Deriving it here again would let the two drift.
      const extent = extentFor(msg.tool, msg.layer === "foreground" ? 1 : 0, msg.rect);
      if (extent.layers.length > 1) {
        // merge: both layers, and Area names only one
        return { kind: "everything" };
      }
      return {
        kind: "pixels", owner,
        // copy writes nothing, but it *reads* the rectangle, so it still has
        // to stay ordered against writes to it
        layer: msg.layer,
        x0: extent.x0,
        y0: extent.y0,
        x1: extent.x1,
        y1: extent.y1,
      };
    }
    case "line":
      return {
        kind: "pixels", owner,
        layer: msg.layer,
        x0: Math.min(msg.from.x, msg.to.x) - msg.brushSize,
        y0: Math.min(msg.from.y, msg.to.y) - msg.brushSize,
        x1: Math.max(msg.from.x, msg.to.x) + msg.brushSize,
        y1: Math.max(msg.from.y, msg.to.y) + msg.brushSize,
      };
    case "bezier": {
      // The control polygon bounds the curve, so its box padded by the brush
      // is a superset of what the curve can touch.
      const xs = msg.points.filter((_, i) => i % 2 === 0);
      const ys = msg.points.filter((_, i) => i % 2 === 1);
      return {
        kind: "pixels", owner,
        layer: msg.layer,
        x0: Math.min(...xs) - msg.brushSize,
        y0: Math.min(...ys) - msg.brushSize,
        x1: Math.max(...xs) + msg.brushSize,
        y1: Math.max(...ys) + msg.brushSize,
      };
    }
    case "fillRegion":
      // The rectangle it covers, and no more. A seeded fill had to claim the
      // whole layer, because where a flood reaches is not known until it is
      // run; pixels know exactly where they go, so two fills in different
      // corners no longer make each other replay.
      return {
        kind: "pixels", owner,
        layer: msg.layer,
        x0: msg.x,
        y0: msg.y,
        x1: msg.x + msg.width - 1,
        y1: msg.y + msg.height - 1,
      };
    case "text":
      // Glyph metrics are the font's business, not ours, and guessing a box
      // too small would let a concurrent op reorder through it. Take the
      // layer.
      return { kind: "pixels", owner, layer: msg.layer, x0: -Infinity, y0: -Infinity, x1: Infinity, y1: Infinity };
    case "eraseAll":
    case "fill":
    case "snapshot":
      // Fills depend on (and snapshots replace) the whole layer
      return { kind: "pixels", owner, layer: msg.layer, x0: -Infinity, y0: -Infinity, x1: Infinity, y1: Infinity };
    case "undoPoint":
      return { kind: "user" };
    default:
      return { kind: "everything" };
  }
}

function areasConcurrent(a: Area, b: Area): boolean {
  if (a.kind === "user" || b.kind === "user") return true;
  if (a.kind === "everything" || b.kind === "everything") return false;
  // Different layer pairs cannot contend, however many people are drawing:
  // the usual case, where each participant paints into their own. Two
  // operations aimed at the *same* pair still fall through to the rectangle
  // test below, so drawing on somebody else's layer is reconciled exactly as
  // drawing on your own always was.
  if (a.owner !== b.owner) return true;
  if (a.layer !== b.layer) return true;
  return a.x1 < b.x0 || b.x1 < a.x0 || a.y1 < b.y0 || b.y1 < a.y0;
}

export class CanvasHistory {
  private engine: DrawingEngine;
  // Whoever the host says we are on the canonical stream -- the 1-byte
  // session id the server assigns, however it names it. -1 until told.
  private localUserId: HistoryActorId = -1;
  /** Whose pair local marks land in; our own unless the host redirects it. */
  private localTargetOwner: ActorKey = actorKey(-1);
  /**
   * The stroke the pointer is painting, which this history cannot see yet.
   *
   * A controlled painter draws straight into the engine and hands its stroke
   * over in chunks, so for up to a chunk's worth of time there are pixels on
   * the canvas that no entry here accounts for. A replay would restore a
   * savepoint over them and nothing would put them back -- the chunk arrives
   * afterwards through `registerOptimisticOperation`, which paints nothing,
   * and its echo is taken as already on screen -- leaving a gap only its
   * author sees. A savepoint taken then would hold them in a position they
   * come after, and draw them twice on the next replay.
   */
  private localWork: { drawing: () => boolean; flush: () => void } | null = null;
  private onChange?: (canUndo: boolean, canRedo: boolean) => void;

  private entries: Entry[] = [];
  private savepoints: Savepoint[] = [];
  /** Who has drawn since the last savepoint, so the rest can share its arrays. */
  private releaseRenames?: () => void;
  private fork: ForkEntry[] = [];
  /**
   * Canonical messages seen while the fork has been waiting for its echoes,
   * against `MAX_FORK_FALLBEHIND`. Reset whenever the fork empties.
   */
  private forkFallbehind = 0;
  /** A bounded ring of recent decisions; see `HistoryTraceEvent`. */
  private trace: HistoryTraceEvent[] = [];
  private readonly startedAt = Date.now();
  private canonicalLog: CanonicalPainterOperation[] = [];
  private openBatch: OpenBatch | null = null;
  // Per-user stroke continuation state of the currently rendered canvas
  private liveStrokes = new StrokeStates();
  private snapshotCache = new WeakMap<HistorySnapshot, Uint8ClampedArray>();
  /**
   * Decoded raster marks, so a replay does not decode the same PNG again for
   * every pass over the history it appears in.
   */
  private coverageCache = new WeakMap<HistoryFillRegion, Uint8Array>();

  /** What this history's savepoints may weigh; see `MAX_SAVEPOINT_BYTES`. */
  private readonly maxSavepointBytes: number;

  constructor(
    engine: DrawingEngine,
    onChange?: (canUndo: boolean, canRedo: boolean) => void,
    options?: {
      /**
       * Overrides the savepoint memory budget. A host that knows it is on a
       * device with less to spare can lower it; the cost is a longer replay
       * when history is rebuilt from an older position.
       */
      maxSavepointBytes?: number;
    }
  ) {
    this.engine = engine;
    this.onChange = onChange;
    this.maxSavepointBytes = options?.maxSavepointBytes ?? MAX_SAVEPOINT_BYTES;
    // Savepoints hold a participant's layers under their name, and that name
    // changes when the server names them. Following the rename is what stops a
    // restore from conjuring the old name back into existence as a
    // participant of its own, holding whatever those layers held at the time.
    this.releaseRenames = engine.onOwnerRenamed((from, to) => {
      for (const savepoint of this.savepoints) {
        const held = savepoint.layers.get(from);
        if (held === undefined) continue;
        savepoint.layers.delete(from);
        savepoint.layers.set(to, held);
      }
    });
    this.reset();
  }

  /** Stops following the engine's renames. */
  dispose(): void {
    this.releaseRenames?.();
    this.releaseRenames = undefined;
  }

  setLocalUserId(id: HistoryActorId): void {
    const wasOwn = this.localTargetOwner === actorKey(this.localUserId);
    this.localUserId = id;
    // Aiming at our own layers is the default, and stays true through the
    // rename a collaborative client does when the server names it.
    if (wasOwn) this.localTargetOwner = actorKey(id);
  }

  /**
   * Point new local marks at a participant's layers.
   *
   * Anyone may draw into anyone's pair, so this is what the toolbox sets when
   * a participant picks whose layers to work on. It only affects marks made
   * from here on; a stroke already open keeps the pair it was started in.
   */
  setLocalTargetOwner(owner: HistoryActorId): void {
    this.localTargetOwner = actorKey(owner);
  }

  /** Whose layers new local marks land in. */
  getLocalTargetOwner(): ActorKey {
    return this.localTargetOwner;
  }

  /**
   * Blanks the canvas, then resets history against it.
   *
   * Used when a reconnect is about to replay the whole canonical history:
   * `reset()` alone bases everything on whatever is currently on screen, so
   * replaying on top of the pre-disconnect pixels would apply every stroke a
   * second time — invisible for opaque brushes, not for translucent ones.
   */
  resetToBlankCanvas(): void {
    for (const owner of this.engine.ownerIds()) {
      const layers = this.engine.layersFor(owner);
      layers.foreground.fill(0);
      layers.background.fill(0);
    }
    this.reset();
    this.queueUpdates();
  }

  /** Clears everything and takes a base savepoint of the current layers. */
  reset(): void {
    this.entries = [];
    this.fork = [];
    this.canonicalLog = [];
    this.openBatch = null;
    // Copies survive a checkpoint, as they do in the room: a paste after it
    // still reads what its author copied before it.
    const clipboards = this.liveStrokes.clipboards;
    this.liveStrokes = new StrokeStates();
    for (const [userId, clipboard] of clipboards) {
      this.liveStrokes.clipboards.set(userId, clipboard);
    }
    // Cleared before capturing, not after: `captureLayers` shares the last
    // savepoint's arrays for anyone who has not drawn since it, and with the
    // dirty set just emptied that is everyone. Leaving the old savepoints in
    // place here would base the new one on the pixels of the old one rather
    // than on the canvas actually in front of us.
    // Emptied before capturing: with no previous savepoint there is nothing
    // to share with, so this one is copied from the canvas in front of us
    // rather than from whatever the old one held.
    this.savepoints = [];
    const captured = this.captureLayers();
    this.savepoints = [
      {
        index: 0,
        layers: captured.layers,
        generations: captured.generations,
        strokes: cloneStrokes(this.liveStrokes),
      },
    ];
    this.notify();
  }

  /** Where a live operation's pixels go: its author's own pair. */
  private readonly liveSource: LayerSource = (owner) =>
    this.engine.layersFor(owner);

  /**
   * Copies every participant's current layers for a savepoint, sharing the
   * previous savepoint's arrays for anyone who has not drawn since it.
   */
  private captureLayers(): {
    layers: Map<ActorKey, OwnerLayers>;
    generations: Map<ActorKey, number>;
  } {
    const last = this.savepoints[this.savepoints.length - 1];
    const layers = new Map<ActorKey, OwnerLayers>();
    const generations = new Map<ActorKey, number>();
    for (const owner of this.engine.ownerIds()) {
      const generation = this.engine.layerGeneration(owner);
      generations.set(owner, generation);
      // Unchanged since the last savepoint took its copy, so that copy still
      // describes them and both savepoints can hold the one array. The engine
      // answers this, so no writer has to remember to.
      const unchanged =
        last !== undefined && last.generations.get(owner) === generation;
      const shared = unchanged ? last.layers.get(owner) : undefined;
      if (shared) {
        layers.set(owner, shared);
        continue;
      }
      const live = this.engine.layersFor(owner);
      layers.set(owner, {
        foreground: new Uint8ClampedArray(live.foreground),
        background: new Uint8ClampedArray(live.background),
      });
    }
    return { layers, generations };
  }

  /**
   * Puts every participant's layers back to a savepoint.
   *
   * Anyone who has no entry in it joined after it was taken, so their pair is
   * blanked rather than left showing work the replay is about to redraw.
   */
  private restoreLayers(sp: Savepoint): void {
    // Anyone the savepoint knows about must exist before the sweep below, or
    // a participant who left and whose pair was released never comes back.
    for (const owner of sp.layers.keys()) this.engine.layersFor(owner);
    for (const owner of this.engine.ownerIds()) {
      const live = this.engine.layersFor(owner);
      const saved = sp.layers.get(owner);
      if (saved) {
        live.foreground.set(saved.foreground);
        live.background.set(saved.background);
      } else {
        live.foreground.fill(0);
        live.background.fill(0);
      }
    }
  }

  /** True while any locally drawn state is not yet server-confirmed. */
  get hasPendingLocal(): boolean {
    return (
      this.fork.length > 0 ||
      (this.openBatch !== null && this.openBatch.points.length > 0)
    );
  }

  canUndo(): boolean {
    return this.projectedUndoCounts().done > 0;
  }

  canRedo(): boolean {
    return this.projectedUndoCounts().undone > 0;
  }

  /**
   * Counts the local user's undoable/redoable strokes, projecting the effect
   * of unconfirmed fork messages. Undo takes effect only on server echo, so
   * without this projection a quickly repeated undo click would send multiple
   * UNDO messages and revert more strokes than intended.
   */
  private projectedUndoCounts(): { done: number; undone: number } {
    let done = 0;
    let undone = 0;
    for (const e of this.entries) {
      if (
        e.msg.type === "undoPoint" &&
        actorKey(e.msg.userId) === actorKey(this.localUserId)
      ) {
        if (e.undo === "done") done++;
        else if (e.undo === "undone") undone++;
      }
    }
    for (const f of this.fork) {
      if (f.msg.type === "undoPoint") {
        // A new stroke will be undoable and kills the redo stack
        done++;
        undone = 0;
      } else if (f.msg.type === "undo") {
        if (f.msg.redo) {
          if (undone > 0) {
            undone--;
            done++;
          }
        } else if (done > 0) {
          done--;
          undone++;
        }
      }
    }
    return { done, undone };
  }

  /**
   * Handles a locally generated message: queues it in the fork (to be matched
   * against the server's echo) and applies drawing messages optimistically.
   * UNDO and UNDO_POINT only take effect when echoed, like in Drawpile.
   */
  handleLocal(bytes: ArrayBuffer, msg: HistoryOperation): void {
    if (msg.type === "snapshot") {
      throw new Error("Snapshots cannot be optimistic local operations");
    }
    this.handleLocalOperation({
      id: transportId(new Uint8Array(bytes)),
      actorId: String(msg.userId),
      operation: fromHistoryOperation(msg),
    });
  }

  /** Applies an optimistic operation emitted through the public API. */
  handleLocalOperation(entry: LocalPainterOperation): void {
    const msg = toHistoryOperation(entry.actorId, entry.operation);
    this.record({ source: "local", op: msg.type, actor: entry.actorId });
    this.fork.push({
      id: entry.id,
      msg,
      area: affectedArea(msg),
    });
    if (msg.type === "undoPoint") {
      // Reset the local stroke continuation so a new stroke doesn't dedup
      // against the previous one's endpoint
      this.liveStrokes.set(actorKey(msg.userId), null);
    } else if (msg.type !== "undo") {
      this.applyDrawSync(msg, this.liveSource, this.liveStrokes);
    }
    // Update undo/redo button state immediately (projected over the fork)
    this.notify();
  }

  /**
   * Registers an operation the interactive canvas has already painted.
   * Controlled mounts use this path because pointer latency stays local while
   * canonical ordering remains owned by this history.
   */
  registerOptimisticOperation(entry: LocalPainterOperation): void {
    const msg = toHistoryOperation(entry.actorId, entry.operation);
    this.record({ source: "local", op: msg.type, actor: entry.actorId });
    this.fork.push({ id: entry.id, msg, area: affectedArea(msg) });
    if (msg.type === "undoPoint") {
      this.liveStrokes.set(actorKey(entry.actorId), null);
    } else if (msg.type === "region" && msg.tool === "copy") {
      // The copy was made on the canvas, not here, so the engine holds it and
      // this history does not. Without it, a remote op that swapped clipboards
      // before the paste arrived would find nothing to hand back.
      this.liveStrokes.clipboards.set(actorKey(entry.actorId), this.engine.getClipboard());
    }
    this.notify();
  }

  /**
   * Appends one point to the open local stroke batch, painting it
   * immediately. The batch becomes a single STROKE fork entry on flush.
   * Returns the number of accumulated points (so the caller can force a
   * flush on large batches).
   */
  addLocalSegment(
    layer: LayerName,
    brushSize: number,
    brushType: WireBrushType,
    color: { r: number; g: number; b: number; a: number },
    x: number,
    y: number,
    mask: Mask = NO_MASK
  ): number {
    if (this.localUserId === -1) return 0;
    if (!this.openBatch) {
      this.openBatch = {
        targetOwner: this.localTargetOwner,
        layer,
        brushSize,
        brushType,
        color,
        mask,
        points: [],
        area: {
          kind: "pixels",
          owner: this.localTargetOwner,
          layer,
          x0: x - brushSize,
          y0: y - brushSize,
          x1: x + brushSize,
          y1: y + brushSize,
        },
      };
    }
    const batch = this.openBatch;
    batch.points.push({ x, y });
    batch.area.x0 = Math.min(batch.area.x0, x - brushSize);
    batch.area.y0 = Math.min(batch.area.y0, y - brushSize);
    batch.area.x1 = Math.max(batch.area.x1, x + brushSize);
    batch.area.y1 = Math.max(batch.area.y1, y + brushSize);

    this.applyStrokePoints(
      this.localUserId,
      batch,
      [{ x, y }],
      this.liveSource(this.localTargetOwner),
      this.liveStrokes
    );
    return batch.points.length;
  }

  /**
   * Closes the open batch and returns its semantic stroke. The transport
   * encodes it, then calls `commitLocalStroke` with the opaque bytes it sent.
   */
  flushLocalStroke(): HistoryStroke | null {
    const batch = this.openBatch;
    this.openBatch = null;
    if (!batch || batch.points.length === 0) return null;
    return {
      type: "stroke",
      userId: this.localUserId,
      targetOwner: batch.targetOwner,
      layer: batch.layer,
      brushSize: batch.brushSize,
      brushType: batch.brushType,
      color: batch.color,
      points: batch.points,
      mask: batch.mask,
    };
  }

  /** Registers the transport token used to recognize this stroke's echo. */
  commitLocalStroke(bytes: ArrayBuffer, msg: HistoryStroke): void {
    this.fork.push({
      id: transportId(new Uint8Array(bytes)),
      msg,
      area: affectedArea(msg),
    });
  }

  /**
   * Handles a message from the canonical server stream (a remote user's
   * message or the echo of a local one).
   */
  async handleRemote(
    raw: Uint8Array,
    msg: HistoryOperation,
    seq?: number
  ): Promise<void> {
    return this.handleCanonicalMessage(transportId(raw), msg, seq);
  }

  /** Applies a server-ordered operation and reconciles any optimistic fork. */
  async handleCanonicalOperation(
    entry: CanonicalPainterOperation,
    sequenceOverride?: number,
  ): Promise<void> {
    const msg = toHistoryOperation(entry.actorId, entry.operation);
    const seq = sequenceOverride ?? entry.sequence;
    await this.handleCanonicalMessage(entry.id, msg, seq);
    this.canonicalLog.push({ ...entry, sequence: seq });
  }

  private record(event: Omit<HistoryTraceEvent, "at">): void {
    this.trace.push({ at: Date.now() - this.startedAt, ...event });
    if (this.trace.length > TRACE_LENGTH) this.trace.shift();
  }

  /**
   * The recent history of what arrived and what was decided about it.
   *
   * For attaching to a report of a canvas that came out wrong: the operations
   * themselves are recoverable from `getCanonicalOperations`, but the order
   * they arrived in relative to local work, and which branch of reconciliation
   * each took, is not recoverable from anything once it has happened.
   */
  synchronizationTrace(): HistoryTraceEvent[] {
    return this.trace.map((event) => ({ ...event }));
  }

  /** Canonical transport-neutral log since the most recent reset/checkpoint. */
  getCanonicalOperations(): CanonicalPainterOperation[] {
    return this.canonicalLog.map((entry) => ({
      ...entry,
      operation: cloneOperation(entry.operation),
    }));
  }

  /** Tells this history how to see and hand over the stroke in progress. */
  setLocalWork(localWork: { drawing: () => boolean; flush: () => void } | null): void {
    this.localWork = localWork;
  }

  /**
   * Puts the stroke in progress into the fork, where replays and the
   * concurrency check can see it. Registration is synchronous, so it is there
   * when this returns.
   */
  private flushLocalWork(): void {
    if (this.localWork?.drawing()) this.localWork.flush();
  }

  private async handleCanonicalMessage(
    id: string,
    msg: HistoryOperation,
    seq?: number,
  ): Promise<void> {
    // Somebody else's mark on the pair the pointer is painting: it has to be
    // checked against the stroke in progress, so that stroke has to be in the
    // fork first. Only then, because a flush is a message for the whole room,
    // and people drawing on their own layers never contend.
    if (
      this.localWork?.drawing() &&
      !("userId" in msg && actorKey(msg.userId) === actorKey(this.localUserId))
    ) {
      const area = affectedArea(msg);
      if (
        area.kind === "everything" ||
        (area.kind === "pixels" && area.owner === this.localTargetOwner)
      ) {
        this.flushLocalWork();
      }
    }

    // Echo of our own fork head: already on the canvas (except undo/undoPoint
    // which take effect now)
    const actor = "userId" in msg ? actorKey(msg.userId) : "";

    if (this.fork.length > 0 && this.fork[0].id === id) {
      this.record({ source: "canonical", op: msg.type, actor, seq, action: "echo" });
      this.fork.shift();
      // Caught up with itself: the fork is keeping pace again.
      if (this.fork.length === 0) this.forkFallbehind = 0;
      if (msg.type === "undo") {
        await this.processUndo(msg.userId, msg.redo);
      } else if (msg.type === "undoPoint") {
        this.appendUndoPoint(msg, seq);
      } else {
        this.entries.push({ seq, msg, undo: "done" });
      }
      this.maybeSavepoint();
      this.notify();
      return;
    }

    // Our own work, echoed back, but not from the head of the fork: whatever
    // sits in front of it was never sequenced. A connection delivers its
    // messages in the order they were sent and the server sequences them as
    // they arrive, so an echo of a later one is proof the earlier ones are
    // never coming -- a send the host declined because the socket was not
    // open, or one that went out on a socket that did not survive.
    //
    // So they are dropped and the rest of the fork kept. Falling through to
    // the divergence below instead cleared the whole fork, which left every
    // echo still in flight arriving to a fork that no longer held it: each one
    // diverged in turn and rebuilt history again, so a single unsent operation
    // cost a savepoint restore, a replay and a repaint of every participant's
    // layers per message for the rest of the stroke.
    const abandoned = this.fork.findIndex((entry) => entry.id === id);
    if (abandoned > 0) {
      this.record({ source: "canonical", op: msg.type, actor, seq, action: "abandoned" });
      console.warn(
        `Canvas history: ${abandoned} local operation(s) never reached the server`
      );
      this.fork.splice(0, abandoned + 1);
      if (this.fork.length === 0) this.forkFallbehind = 0;
      let first = -1;
      if (msg.type === "undo") {
        first = this.markUndo(msg.userId, msg.redo);
      } else if (msg.type === "undoPoint") {
        this.appendUndoPoint(msg, seq);
      } else {
        this.entries.push({ seq, msg, undo: "done" });
      }
      // Once, to take the abandoned operations' pixels off the canvas. The
      // fork that remains is re-applied on top, so the echoes behind this one
      // meet a fork that still has them at its head and cost nothing again.
      await this.replayFrom(
        first >= 0 ? this.savepointFor(first) : this.latestSavepoint()
      );
      this.maybeSavepoint();
      this.notify();
      return;
    }

    // Same user from another connection: the server's order diverged from the
    // fork. Drop the pending local state (in-flight messages re-arrive as
    // ordinary echoes) and unconditionally rebuild from canonical history,
    // since the canvas still shows the dropped fork's optimistic pixels.
    if (
      this.hasPendingLocal &&
      "userId" in msg &&
      actorKey(msg.userId) === actorKey(this.localUserId)
    ) {
      this.record({ source: "canonical", op: msg.type, actor, seq, action: "diverged" });
      console.warn(
        "Canvas history rollback: server order diverged from local fork"
      );
      this.fork = [];
      this.openBatch = null;
      this.forkFallbehind = 0;
      let first = -1;
      if (msg.type === "undo") {
        first = this.markUndo(msg.userId, msg.redo);
      } else if (msg.type === "undoPoint") {
        this.appendUndoPoint(msg, seq);
      } else {
        this.entries.push({ seq, msg, undo: "done" });
      }
      await this.replayFrom(
        first >= 0 ? this.savepointFor(first) : this.latestSavepoint()
      );
      this.maybeSavepoint();
      this.notify();
      return;
    }

    // Remote message while local strokes are unconfirmed: apply directly only
    // if it can't touch any pending area (Drawpile's concurrency check);
    // otherwise rebuild and re-apply the pending local state on top.
    if (this.hasPendingLocal) {
      // Somebody else's message arrived while ours are still unconfirmed. Past
      // the bound the echoes are not coming, and continuing to show pixels
      // only we have is worse than losing them.
      if (++this.forkFallbehind >= MAX_FORK_FALLBEHIND) {
        this.record({ source: "canonical", op: msg.type, actor, seq, action: "fallbehind" });
        console.warn(
          `Canvas history rollback: local fork fell ${this.forkFallbehind} messages behind`
        );
        this.fork = [];
        this.openBatch = null;
        this.forkFallbehind = 0;
        await this.applyCanonical(msg, seq, { replay: true });
        return;
      }

      const area = affectedArea(msg);
      const pendingAreas: Area[] = this.fork.map((f) => f.area);
      if (this.openBatch && this.openBatch.points.length > 0) {
        pendingAreas.push(this.openBatch.area);
      }
      const concurrent = pendingAreas.every((a) => areasConcurrent(a, area));
      this.record({
        source: "canonical", op: msg.type, actor, seq,
        action: concurrent ? "concurrent" : "replay",
      });
      await this.applyCanonical(msg, seq, { replay: !concurrent });
      return;
    }

    this.forkFallbehind = 0;
    this.record({ source: "canonical", op: msg.type, actor, seq, action: "applied" });

    await this.applyCanonical(msg, seq, { replay: false });
  }

  /**
   * Squashes all entries at or below the session reset's base sequence into a
   * new base savepoint. Their undo state is frozen; memory is reclaimed.
   *
   * Says whether there was anything to squash. The layers at the cut are
   * `baseLayers()` afterwards, for whoever wants them -- which is rarely
   * anyone, so they are not copied out on the chance.
   */
  async handleResetPoint(baseSeq: number): Promise<boolean> {
    this.record({
      source: "reset",
      op: "resetPoint",
      actor: "",
      seq: baseSeq,
    });
    let cut = 0;
    while (
      cut < this.entries.length &&
      this.entries[cut].seq !== undefined &&
      (this.entries[cut].seq as number) <= baseSeq
    ) {
      cut++;
    }
    if (cut === 0) return false;

    // Compute the state at the cut into temporary buffers, one pair per
    // participant, allocated as the replay first mentions each of them.
    const sp = this.savepointFor(cut);
    const layers = new Map<ActorKey, OwnerLayers>();
    for (const [owner, saved] of sp.layers) {
      layers.set(owner, {
        foreground: new Uint8ClampedArray(saved.foreground),
        background: new Uint8ClampedArray(saved.background),
      });
    }
    const forkSource: LayerSource = (owner) => {
      let pair = layers.get(owner);
      if (!pair) {
        pair = {
          foreground: new Uint8ClampedArray(this.engine.imageWidth * this.engine.imageHeight * 4),
          background: new Uint8ClampedArray(this.engine.imageWidth * this.engine.imageHeight * 4),
        };
        layers.set(owner, pair);
      }
      return pair;
    };
    const strokes = cloneStrokes(sp.strokes);
    for (let i = sp.index; i < cut; i++) {
      const entry = this.entries[i];
      if (entry.undo === "done") {
        await this.applyMessage(entry.msg, forkSource, strokes);
      }
    }

    this.entries = this.entries.slice(cut);
    this.canonicalLog = this.canonicalLog.filter(
      (entry) => entry.sequence > baseSeq,
    );
    this.savepoints = [
      {
        index: 0,
        layers,
        // These buffers were built here rather than copied from the canvas,
        // so nothing about the engine's counts describes them: recording the
        // current ones would let the next savepoint share arrays that never
        // matched the layers in the first place.
        generations: new Map(),
        strokes,
      },
      ...this.savepoints
        .filter((s) => s.index >= cut)
        .map((s) => ({ ...s, index: s.index - cut })),
    ];
    this.notify();
    return true;
  }

  /**
   * Every participant's layers as of the base savepoint: the last reset
   * point, or the canvas `reset()` found.
   *
   * The savepoint's own arrays, not copies. Nothing writes into a savepoint
   * once it is taken -- restoring one copies out of it -- so they are safe to
   * read, and must not be written.
   */
  baseLayers(): ReadonlyMap<ActorKey, Readonly<OwnerLayers>> {
    return this.savepoints[0].layers;
  }

  private async applyCanonical(
    msg: HistoryOperation,
    seq: number | undefined,
    { replay }: { replay: boolean }
  ): Promise<void> {
    if (msg.type === "undo") {
      await this.processUndo(msg.userId, msg.redo);
    } else if (msg.type === "undoPoint") {
      this.appendUndoPoint(msg, seq);
      this.liveStrokes.set(actorKey(msg.userId), null);
      if (replay) {
        await this.replayFrom(this.latestSavepoint());
      }
    } else {
      this.entries.push({ seq, msg, undo: "done" });
      if (replay) {
        await this.replayFrom(this.latestSavepoint());
      } else {
        await this.applyMessage(msg, this.liveSource, this.liveStrokes);
        // A snapshot is assigned straight into the buffer, so the engine has
        // no region for it; everything else drew through the engine.
        if (msg.type === "snapshot") this.queueUpdates();
        else this.queueDrawnUpdates();
      }
    }
    this.maybeSavepoint();
    this.notify();
  }

  /** Appends an undo point; a new operation by a user kills their redo. */
  private appendUndoPoint(
    msg: Extract<HistoryOperation, { type: "undoPoint" }>,
    seq?: number
  ): void {
    for (const entry of this.entries) {
      if (
        "userId" in entry.msg &&
        actorKey(entry.msg.userId) === actorKey(msg.userId) &&
        entry.undo === "undone"
      ) {
        entry.undo = "gone";
      }
    }
    this.entries.push({ seq, msg, undo: "done" });
  }

  /** Marks entries and replays; no-op if there is nothing to undo/redo. */
  private async processUndo(userId: HistoryActorId, redo: boolean): Promise<void> {
    const first = this.markUndo(userId, redo);
    if (first < 0) return;
    await this.replayFrom(this.savepointFor(first));
  }

  /**
   * Marks entries for an undo or redo by `userId` and returns the first
   * affected history index (-1 if nothing to do).
   *
   * Undo: the user's entries from their most recent active undo point to the
   * end of history become undone. Redo: the user's entries from their
   * earliest undone undo point to their next undo point become done again.
   */
  private markUndo(userId: HistoryActorId, redo: boolean): number {
    let first = -1;
    if (redo) {
      first = this.entries.findIndex(
        (e) =>
          e.msg.type === "undoPoint" &&
          actorKey(e.msg.userId) === actorKey(userId) &&
          e.undo === "undone"
      );
      if (first < 0) return -1;
      let next = this.entries.length;
      for (let i = first + 1; i < this.entries.length; i++) {
        const e = this.entries[i];
        if (
          e.msg.type === "undoPoint" &&
          actorKey(e.msg.userId) === actorKey(userId)
        ) {
          next = i;
          break;
        }
      }
      for (let i = first; i < next; i++) {
        const e = this.entries[i];
        if (
          "userId" in e.msg &&
          actorKey(e.msg.userId) === actorKey(userId) &&
          e.undo === "undone"
        ) {
          e.undo = "done";
        }
      }
    } else {
      for (let i = this.entries.length - 1; i >= 0; i--) {
        const e = this.entries[i];
        if (
          e.msg.type === "undoPoint" &&
          actorKey(e.msg.userId) === actorKey(userId) &&
          e.undo === "done"
        ) {
          first = i;
          break;
        }
      }
      if (first < 0) return -1;
      for (let i = first; i < this.entries.length; i++) {
        const e = this.entries[i];
        if (
          "userId" in e.msg &&
          actorKey(e.msg.userId) === actorKey(userId) &&
          e.undo === "done"
        ) {
          e.undo = "undone";
        }
      }
    }

    // Savepoints past the modified region no longer describe replayable state
    this.savepoints = this.savepoints.filter((s) => s.index <= first);
    return first;
  }

  /**
   * Restores a savepoint, replays all done entries after it, then re-applies
   * the unconfirmed fork on top.
   */
  private async replayFrom(sp: Savepoint): Promise<void> {
    // Before the restore wipes it: in the fork, it is re-applied below.
    this.flushLocalWork();
    this.restoreLayers(sp);
    const strokes = cloneStrokes(sp.strokes);
    for (let i = sp.index; i < this.entries.length; i++) {
      const entry = this.entries[i];
      if (entry.undo !== "done") continue;
      // Yield only for a message whose decode has not happened yet. A rollback
      // between savepoints can span sixty-odd entries, and awaiting each one
      // spent a turn of the event loop apiece to wait for values already in
      // hand -- with the canvas showing the savepoint until the last resolved.
      if (!this.applyMessageSync(entry.msg, this.liveSource, strokes)) {
        await this.applyMessage(entry.msg, this.liveSource, strokes);
      }
    }
    for (const f of this.fork) {
      if (f.msg.type === "undoPoint") {
        // Keyed by the entry's own actor, exactly as the confirmed entries
        // above are, so a boundary always clears the state the strokes
        // beside it are drawn under.
        strokes.set(actorKey(f.msg.userId), null);
      } else if (f.msg.type !== "undo") {
        this.applyDrawSync(f.msg, this.liveSource, strokes);
      }
    }
    if (this.openBatch && this.openBatch.points.length > 0) {
      this.applyStrokePoints(
        this.localUserId,
        this.openBatch,
        this.openBatch.points,
        this.liveSource(this.openBatch.targetOwner),
        strokes
      );
    }
    this.liveStrokes = strokes;
    this.queueUpdates();
  }

  private savepointFor(index: number): Savepoint {
    let best = this.savepoints[0];
    for (const sp of this.savepoints) {
      if (sp.index <= index && sp.index >= best.index) {
        best = sp;
      }
    }
    return best;
  }

  private latestSavepoint(): Savepoint {
    return this.savepoints[this.savepoints.length - 1];
  }

  /** How many savepoints are being kept. */
  savepointCountForTest(): number {
    return this.savepoints.length;
  }

  /** What they weigh between them, shared arrays counted once. */
  savepointBytesForTest(): number {
    return this.savepointBytes();
  }

  /** Forces the savepoint that would otherwise wait for the interval. */
  takeSavepointForTest(): void {
    const captured = this.captureLayers();
    this.savepoints.push({
      index: this.entries.length,
      layers: captured.layers,
      generations: captured.generations,
      strokes: cloneStrokes(this.liveStrokes),
    });
    // Including what a real one does about the ones it displaces.
    this.evictSavepoints();
  }

  /**
   * Drops savepoints until both bounds hold.
   *
   * The base is kept because every entry replays from it, and the newest
   * because every ordinary rollback starts there; between them the
   * second-oldest is always the one worth least.
   */
  private evictSavepoints(): void {
    while (
      this.savepoints.length > 2 &&
      (this.savepoints.length > MAX_SAVEPOINTS ||
        this.savepointBytes() > this.maxSavepointBytes)
    ) {
      this.savepoints.splice(1, 1);
    }
  }

  /**
   * What the savepoints weigh, counting a shared array once.
   *
   * Savepoints hand each other the arrays of participants who have not drawn
   * since the last one, so adding up their layers naively would count the same
   * buffer many times over and evict savepoints that cost nothing to keep.
   */
  private savepointBytes(): number {
    const counted = new Set<Uint8ClampedArray>();
    let bytes = 0;
    for (const savepoint of this.savepoints) {
      for (const pair of savepoint.layers.values()) {
        for (const layer of [pair.background, pair.foreground]) {
          if (counted.has(layer)) continue;
          counted.add(layer);
          bytes += layer.byteLength;
        }
      }
    }
    return bytes;
  }

  private maybeSavepoint(): void {
    // Savepoints must capture confirmed-only state, and a stroke still under
    // the pointer is not even in the fork yet
    if (this.hasPendingLocal || this.localWork?.drawing()) return;
    const last = this.latestSavepoint();
    if (this.entries.length - last.index < SAVEPOINT_INTERVAL) return;
    const captured = this.captureLayers();
    this.savepoints.push({
      index: this.entries.length,
      layers: captured.layers,
      generations: captured.generations,
      strokes: cloneStrokes(this.liveStrokes),
    });
    this.evictSavepoints();
  }

  /** Applies a message to its author's layers (snapshot decode is async). */
  /**
   * Applies a message without yielding, or declines.
   *
   * A snapshot's PNG needs the browser's decoder, and the decoded layer is
   * cached against the message itself. A replay is
   * re-applying messages that were applied once already, so the cache is warm
   * and this answers for all of them; only a message being seen for the first
   * time has to go the long way round.
   *
   * Drawpile batches messages when replaying for the same reason: replay is
   * mostly the same handful of operations over and over, and paying a turn of
   * the event loop for each one is the expensive part, not the drawing.
   */
  private applyMessageSync(
    msg: HistoryOperation,
    source: LayerSource,
    strokes: StrokeStates
  ): boolean {
    if (msg.type === "snapshot") {
      const data = this.snapshotCache.get(msg);
      if (!data) return false;
      source(actorKey(msg.targetOwner))[msg.layer].set(data);
      return true;
    }
    if (msg.type === "fillRegion") {
      let coverage = this.coverageCache.get(msg);
      if (!coverage) {
        coverage = inflateCoverage(msg.coverage, msg.width, msg.height);
        this.coverageCache.set(msg, coverage);
      }
      this.engine.paintCoveredPixels(
        source(actorKey(msg.targetOwner))[msg.layer],
        msg.x, msg.y, msg.width, msg.height, msg.color, coverage,
      );
      return true;
    }
    if (msg.type === "undoPoint") {
      strokes.set(actorKey(msg.userId), null);
      return true;
    }
    this.applyDrawSync(msg, source, strokes);
    return true;
  }

  private async applyMessage(
    msg: HistoryOperation,
    source: LayerSource,
    strokes: StrokeStates
  ): Promise<void> {
    if (this.applyMessageSync(msg, source, strokes)) return;

    // Decode into the cache, then take the path above. Only a snapshot's PNG
    // needs the browser's decoder; a fill's coverage inflates in place.
    if (msg.type === "snapshot") {
      this.snapshotCache.set(
        msg,
        await pngDataToLayer(
          msg.pngData,
          this.engine.imageWidth,
          this.engine.imageHeight
        )
      );
    }
    this.applyMessageSync(msg, source, strokes);
  }

  /** Points the engine at a mask, or at none. */
  private useMask(mask: Mask | undefined): void {
    this.engine.maskType = mask?.type ?? 0;
    if (mask && mask.type !== 0) {
      this.engine.maskColor = [mask.r, mask.g, mask.b];
    }
  }

  /**
   * Applies one operation, leaving the engine's pen as it found it.
   *
   * The engine has one pen -- continuation state, mask, clipboard -- and the
   * person at this client is holding it: their stroke paints straight into
   * the engine as the pointer moves, between the messages that arrive here.
   * Every operation below borrows the pen for its author and must hand it
   * back, or the local stroke's next segment continues from somebody else's
   * endpoint and joins differently here than on every other client, and a
   * paste in progress drops whatever a remote region op last copied.
   */
  private applyDrawSync(
    msg: HistoryOperation,
    source: LayerSource,
    strokes: StrokeStates
  ): void {
    const pen = {
      stroke: this.engine.getStrokeState(),
      maskType: this.engine.maskType,
      maskColor: this.engine.maskColor,
      clipboard: this.engine.getClipboard(),
    };
    try {
      this.applyDrawWithPen(msg, source, strokes);
    } finally {
      this.engine.setStrokeState(pen.stroke);
      this.engine.maskType = pen.maskType;
      this.engine.maskColor = pen.maskColor;
      this.engine.setClipboard(pen.clipboard);
    }
  }

  private applyDrawWithPen(
    msg: HistoryOperation,
    source: LayerSource,
    strokes: StrokeStates
  ): void {
    // Every drawing message carries the mask it was drawn through, so replay
    // uses the sender's mask rather than whatever this client last set.
    this.useMask("mask" in msg ? msg.mask : undefined);
    // The layers written are the target's. Everything else about the message
    // still follows its author: the pen's continuation state, the clipboard a
    // paste reads, and the undo stack it lands on. The two undo messages write
    // nothing and never reach here.
    const layers = source(
      actorKey("targetOwner" in msg ? msg.targetOwner : msg.userId)
    );
    switch (msg.type) {
      case "stroke":
        this.applyStrokePoints(msg.userId, msg, msg.points, layers, strokes);
        break;
      case "fill":
        this.engine.doFloodFill(
          layers[msg.layer],
          msg.x,
          msg.y,
          msg.color.r,
          msg.color.g,
          msg.color.b,
          msg.color.a
        );
        break;
      case "region": {
        // The clipboard belongs to whoever copied. Every client keeps one per
        // user and swaps it in around the op, so a paste reproduces what the
        // sender copied rather than whatever the receiver last copied -- which
        // is usually nothing.
        const targets = layers;
        this.engine.setClipboard(strokes.clipboards.get(actorKey(msg.userId)) ?? null);
        this.engine.applyRegionTool(
          msg.tool, msg.layer, msg.rect, msg.color, msg.brushSize, targets
        );
        if (msg.tool === "copy") {
          strokes.clipboards.set(actorKey(msg.userId), this.engine.getClipboard());
        }
        break;
      }
      case "line":
        // A line is a stroke of its own, so it starts with no joint to skip,
        // as NEO's line tool does -- not wherever this client's pen last was.
        this.engine.setStrokeState(null);
        this.engine.drawLine(
          layers[msg.layer],
          // Drawn new -> previous, as NEO draws every segment
          msg.to.x, msg.to.y, msg.from.x, msg.from.y,
          msg.brushSize, msg.brushType,
          msg.color.r, msg.color.g, msg.color.b, msg.color.a
        );
        this.engine.setStrokeState(null);
        break;
      case "bezier":
        this.engine.setStrokeState(null);
        this.engine.drawBezier(
          msg.layer,
          msg.points as [number, number, number, number, number, number, number, number],
          msg.brushSize, msg.brushType, msg.color,
          layers[msg.layer]
        );
        break;
      case "eraseAll":
        this.engine.eraseAll(msg.layer, layers);
        break;
      case "text":
        this.engine.drawText(
          msg.layer, msg.x, msg.y,
          msg.color, msg.color.a / 255, msg.text,
          String(fontSizeForBrush(msg.brushSize)), TEXT_FONT_FAMILY,
          layers[msg.layer]
        );
        break;
    }
  }

  /**
   * Applies polyline points for one user. Each point continues from the
   * user's previous point (tracked in `strokes`); a null continuation state
   * (set by their UNDO_POINT) starts a new stroke with a dot.
   */
  private applyStrokePoints(
    userId: HistoryActorId,
    props: Pick<HistoryStroke, "layer" | "brushSize" | "brushType" | "color" | "mask">,
    points: { x: number; y: number }[],
    layers: OwnerLayers,
    strokes: StrokeStates
  ): void {
    // Reached directly by the local live path and by rebuilds, not only
    // through applyDrawSync, so it sets the mask itself.
    this.useMask(props.mask);
    this.engine.setStrokeState(strokes.get(actorKey(userId)) ?? null);
    for (const p of points) {
      const prev = this.engine.getStrokeState();
      // Segments run new -> previous, as NEO draws them. The engine records
      // prevLine as [first arg, second arg], so the most recent point of the
      // last segment is prev[0].
      const [fromX, fromY] = prev === null ? [p.x, p.y] : prev[0];
      this.engine.drawLine(
        layers[props.layer],
        p.x,
        p.y,
        fromX,
        fromY,
        props.brushSize,
        props.brushType,
        props.color.r,
        props.color.g,
        props.color.b,
        props.color.a
      );
    }
    strokes.set(actorKey(userId), this.engine.getStrokeState());
  }

  /**
   * Repaints both layers whole. For the paths that put pixels somewhere the
   * engine cannot have seen: a savepoint restored under a replay, a decoded
   * snapshot, a blanked canvas.
   */
  private queueUpdates(): void {
    // Every participant's, not just our own. A replay rewrites whoever's
    // layers the entries touch, and an undo of somebody else's fill rewrites
    // theirs: repainting only ours would leave their canvas showing pixels
    // the buffer behind it no longer has.
    for (const owner of this.engine.ownerIds()) {
      this.engine.queueLayerUpdate("foreground", owner);
      this.engine.queueLayerUpdate("background", owner);
    }
  }

  /**
   * Repaints only what the operation just drew.
   *
   * The live application path -- one message, one small mark. Every write it
   * makes goes through the engine's own surfaces, which is what makes the
   * region trustworthy; a remote stroke segment covers a few hundred pixels,
   * and uploading the whole canvas for it costs an order of magnitude more
   * than drawing it did.
   */
  private queueDrawnUpdates(): void {
    // No owner loop here: every write on this path went through the engine's
    // own surfaces, which resolve the participant from the buffer and have
    // already marked the right canvas dirty. Only the whole-layer path above,
    // which puts pixels somewhere the engine never saw, has to name them.
    this.engine.queueLayerRegionUpdate("foreground");
    this.engine.queueLayerRegionUpdate("background");
  }

  private notify(): void {
    if (!this.onChange) return;
    // One scan, not two: `canUndo` and `canRedo` are the same walk over every
    // entry since the last checkpoint, and this runs on every message the
    // canonical stream delivers.
    const { done, undone } = this.projectedUndoCounts();
    this.onChange(done > 0, undone > 0);
  }
}
