import type { Mask } from "../../neo/mask";
import type { BrushType } from "../../types/drawing";
import type { RegionTool } from "../../neo/tools";
import type { RegionRect } from "../../neo/regionDrag";

export type Layer = "foreground" | "background";
export type Point = { x: number; y: number };
export type Rgba = { r: number; g: number; b: number; a: number };

/**
 * Where the marks a gesture makes are written down.
 *
 * The drawing hook rasterises every gesture the same way whoever is
 * listening; what differs by mode is only what becomes of the mark
 * afterwards. Offline it is a frame in the `.pch` replay and an entry in the
 * snapshot undo stack. In a session it is an operation for the room, and
 * undo is a message the room replays. Each mode is one class here, and the
 * hook holds exactly one of them -- there is no flag to check per call site
 * and so no call site that can check it wrong.
 *
 * Every mark carries the layer and mask it was drawn through, taken from the
 * settings the drawing hook froze at the press. A sink keeps no copy of its
 * own.
 */
export interface DrawingSink {
  /** A freehand or fill gesture began. */
  pointerDown(): void;
  segment(
    from: Point,
    to: Point,
    brushSize: number,
    brushType: BrushType,
    color: Rgba,
    layer: Layer,
    mask: Mask,
  ): void;
  dot(
    at: Point,
    brushSize: number,
    brushType: BrushType,
    color: Rgba,
    layer: Layer,
    mask: Mask,
  ): void;
  /**
   * A flood was asked for at `at`. Returns true when the sink flooded the
   * layer itself, so the hook does not flood the same seed again.
   */
  fill(at: Point, color: Rgba, layer: Layer, mask: Mask): boolean;
  eraseAll(layer: Layer): void;
  line(
    from: Point,
    to: Point,
    brushSize: number,
    brushType: BrushType,
    color: Rgba,
    layer: Layer,
    mask: Mask,
  ): void;
  bezier(
    points: number[],
    brushSize: number,
    brushType: BrushType,
    color: Rgba,
    layer: Layer,
    mask: Mask,
  ): void;
  region(
    tool: RegionTool,
    layer: Layer,
    rect: RegionRect,
    color: Rgba,
    brushSize: number,
    mask: Mask,
  ): void;
  paste(
    layer: Layer,
    source: RegionRect,
    dx: number,
    dy: number,
    color: Rgba,
    brushSize: number,
    mask: Mask,
  ): void;
  /** The gesture ended; anything held back for it goes out now. */
  pointerUp(): void;
}

/** Whole numbers in [0, 255]; NaN and Infinity become 0. */
export const clampByte = (value: number): number =>
  Math.max(0, Math.min(255, Math.floor(value || 0)));

export const clampRgba = (color: Rgba): Rgba => ({
  r: clampByte(color.r),
  g: clampByte(color.g),
  b: clampByte(color.b),
  a: clampByte(color.a),
});
