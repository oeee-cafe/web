import { deflateCoverage } from "../../utils/rasterCodec";
import type { Mask } from "../../neo/mask";
import type { BrushType } from "../../types/drawing";
import type { RegionTool } from "../../neo/tools";
import type { RegionRect } from "../../neo/regionDrag";
import type { PainterBrush, PainterOperation } from "../../operations";
import type { DrawingEngine } from "../../DrawingEngine";
import type { DrawingSink, Layer, Point, Rgba } from "./DrawingSink";

/**
 * How many points a broadcast stroke may gather before it is sent.
 *
 * A ceiling rather than a target: the interval below usually reaches first.
 * It exists so a very fast pointer cannot build an unbounded message.
 */
const STROKE_CHUNK_POINTS = 32;

/**
 * How long a broadcast stroke may gather for.
 *
 * This is the delay before the rest of the room sees ink that is already on
 * the author's own canvas. Short enough to read as live, long enough to turn
 * a stroke's worth of samples into a handful of messages instead of one per
 * sample.
 */
const STROKE_CHUNK_MS = 50;

type StrokeOperation = Extract<PainterOperation, { kind: "stroke" }>;

/**
 * The session painter's sink: operations for the room's canonical stream.
 *
 * Every gesture becomes an operation the host sends and every client
 * applies, and undo is one of them. Freehand segments are gathered into
 * chunks: every segment used to go out on its own, so a six-second stroke
 * became some hundreds of canonical messages -- hundreds of sequence
 * numbers, history entries and fork entries for one gesture, and the cost of
 * that lands on the whole room's catch-up and on how often the server has to
 * ask for a checkpoint. Drawpile packs thousands of dabs into a message for
 * exactly this reason.
 *
 * The local canvas is painted by the interactive path as the pointer moves,
 * so nothing here delays what the person drawing sees; the wait is only
 * before other people see it, which is what the flush interval bounds.
 */
export class SessionEmitter implements DrawingSink {
  /**
   * Segments of the stroke in progress that have been drawn locally but not
   * yet broadcast, and when the chunk they belong to was opened.
   */
  private chunk:
    | (StrokeOperation & {
        openedAt: number;
        /**
         * True when `points[0]` has already been broadcast and is only here
         * so the next message joins onto it. Without this a dot -- one point,
         * never sent -- looks the same as a chunk holding nothing new.
         */
        carried: boolean;
      })
    | null = null;
  /** Whether the stroke in progress has sent its undo boundary yet. */
  private boundarySent = false;
  private readonly send: (operation: PainterOperation) => void;
  /** The engine, for a fill's coverage; null before it exists. */
  private readonly engine: () => DrawingEngine | null;
  /** Told when a gesture ends, for the host's pointer bookkeeping. */
  private readonly onPointerRelease: (() => void) | undefined;

  constructor(
    send: (operation: PainterOperation) => void,
    engine: () => DrawingEngine | null,
    onPointerRelease?: () => void,
  ) {
    this.send = send;
    this.engine = engine;
    this.onPointerRelease = onPointerRelease;
  }

  /** Sends an operation, preceded by the undo boundary it is one step of. */
  emit(operation: PainterOperation): void {
    if (operation.kind !== "undo" && operation.kind !== "undo-boundary") {
      this.send({ kind: "undo-boundary" });
    }
    this.send(operation);
  }

  undo(redo: boolean): void {
    this.send({ kind: "undo", redo });
  }

  pointerDown(): void {
    this.chunk = null;
    this.boundarySent = false;
  }

  /**
   * Sends the segments accumulated so far as one stroke, and leaves the chunk
   * open at the point they ended on.
   *
   * The next chunk starts from that same point so the line joins across the
   * boundary, which is how consecutive stroke messages have always met.
   */
  flush(): void {
    const chunk = this.chunk;
    if (!chunk) return;
    if (chunk.points.length <= (chunk.carried ? 1 : 0)) return;
    if (!this.boundarySent) {
      this.send({ kind: "undo-boundary" });
      this.boundarySent = true;
    }
    this.send({
      kind: "stroke",
      layer: chunk.layer,
      brushSize: chunk.brushSize,
      brush: chunk.brush,
      color: chunk.color,
      mask: chunk.mask,
      points: chunk.points.slice(),
      ...(chunk.targetActorId === undefined ? {} : { targetActorId: chunk.targetActorId }),
    });
    const last = chunk.points[chunk.points.length - 1];
    chunk.points = [last];
    chunk.carried = true;
    chunk.openedAt = performance.now();
  }

  /**
   * Adds a drawn stroke to the chunk being broadcast, flushing when the chunk
   * is big enough or old enough.
   */
  private gather(operation: StrokeOperation): void {
    if (!this.chunk) {
      this.chunk = {
        ...operation,
        points: operation.points.slice(),
        openedAt: performance.now(),
        carried: false,
      };
    } else {
      // The segment starts where the last one ended, so only its far end is
      // new to the chunk.
      this.chunk.points.push(operation.points[operation.points.length - 1]);
    }
    const open = this.chunk;
    if (
      open.points.length >= STROKE_CHUNK_POINTS ||
      performance.now() - open.openedAt >= STROKE_CHUNK_MS
    ) {
      this.flush();
    }
  }

  segment(
    from: Point,
    to: Point,
    brushSize: number,
    brushType: BrushType,
    color: Rgba,
    layer: Layer,
    mask: Mask,
  ): void {
    this.gather({
      kind: "stroke", layer, brushSize, brush: brushType as PainterBrush,
      color, points: [from, to], mask,
    });
  }

  dot(
    at: Point,
    brushSize: number,
    brushType: BrushType,
    color: Rgba,
    layer: Layer,
    mask: Mask,
  ): void {
    this.gather({
      kind: "stroke", layer, brushSize, brush: brushType as PainterBrush,
      color, points: [at], mask,
    });
  }

  /**
   * Floods the layer here and sends what the flood covered. A seed replayed
   * elsewhere floods whatever that layer holds at the time, which after an
   * undo underneath it is not the same shape at all.
   *
   * In the same turn as the flood, so the operation is in the fork before
   * anything else can be emitted or a checkpoint asked for.
   */
  fill(at: Point, color: Rgba, layer: Layer, mask: Mask): boolean {
    const engine = this.engine();
    if (!engine) return false;
    const target = engine.drawTarget[layer];
    const region = engine.floodFillCapturingRegion(
      target, at.x, at.y, color.r, color.g, color.b, color.a,
    );
    // The layer is flooded either way; a flood that covered nothing sends
    // nothing.
    if (!region) return true;
    const { x, y, width, height, coverage } = region;
    this.emit({
      kind: "fill-region",
      layer,
      at: { x, y },
      width,
      height,
      color,
      coverage: deflateCoverage(coverage),
      mask,
    });
    return true;
  }

  eraseAll(layer: Layer): void {
    this.emit({ kind: "clear-layer", layer });
  }

  line(
    from: Point,
    to: Point,
    brushSize: number,
    brushType: BrushType,
    color: Rgba,
    layer: Layer,
    mask: Mask,
  ): void {
    this.emit({
      kind: "line", layer, brushSize, brush: brushType as PainterBrush,
      color, from, to, mask,
    });
  }

  bezier(
    points: number[],
    brushSize: number,
    brushType: BrushType,
    color: Rgba,
    layer: Layer,
    mask: Mask,
  ): void {
    this.emit({
      kind: "bezier", layer, brushSize, brush: brushType as PainterBrush, color,
      points: points as [number, number, number, number, number, number, number, number],
      mask,
    });
  }

  region(
    tool: RegionTool,
    layer: Layer,
    rect: RegionRect,
    color: Rgba,
    brushSize: number,
    mask: Mask,
  ): void {
    this.emit({ kind: "region", layer, tool, rect, color, brushSize, mask });
  }

  paste(
    layer: Layer,
    source: RegionRect,
    dx: number,
    dy: number,
    color: Rgba,
    brushSize: number,
    mask: Mask,
  ): void {
    this.emit({
      kind: "region",
      layer,
      tool: "paste",
      rect: { x: source.x + dx, y: source.y + dy, width: source.width, height: source.height },
      color,
      brushSize,
      mask,
    });
  }

  pointerUp(): void {
    // Whatever the last chunk did not reach a threshold with still has to go
    // out, or the tail of every stroke would be visible only to its author.
    this.flush();
    this.chunk = null;
    this.onPointerRelease?.();
  }
}
