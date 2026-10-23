import { ActionRecorder, type ActionItem } from "../../utils/ActionRecorder";
import type { Mask } from "../../neo/mask";
import type { BrushType } from "../../types/drawing";
import { fillToolTypeFor, frameShapeFor, type RegionTool } from "../../neo/tools";
import type { RegionRect } from "../../neo/regionDrag";
import type { DrawingEngine } from "../../DrawingEngine";
import { layerToPngDataUrl } from "../../utils/layerDataUrl";
import type { DrawingSink, Layer, Point, Rgba } from "./DrawingSink";

// Constants matching Neo's LINETYPE values
const LINETYPE_PEN = 1;
const LINETYPE_ERASER = 2;
const LINETYPE_BRUSH = 3;
const LINETYPE_TONE = 4;
const LINETYPE_DODGE = 5;
const LINETYPE_BURN = 6;
const LINETYPE_BLUR = 7;

/** The line type NEO serialises for each stroked brush. */
export const lineTypeForBrush = (brushType: BrushType): number => {
  switch (brushType) {
    case "eraser":
      return LINETYPE_ERASER;
    case "brush":
      return LINETYPE_BRUSH;
    case "halftone":
      return LINETYPE_TONE;
    case "dodge":
      return LINETYPE_DODGE;
    case "burn":
      return LINETYPE_BURN;
    case "blur":
      return LINETYPE_BLUR;
    default:
      // solid is the remaining drawing brush; fill and pan do not serialize
      // as strokes, but keeping their historical fallback is harmless.
      return LINETYPE_PEN;
  }
};

const layerIndex = (layer: Layer): number => (layer === "foreground" ? 1 : 0);

/**
 * The offline painter's sink: NEO's `.pch` replay, one frame per gesture.
 *
 * The frames are NEO's own, laid out as `ActionRecorder` documents, and the
 * replay is checked against NEO itself in the browser tests. Undo here is
 * the snapshot stack the drawing hook keeps, with the recorder's head moved
 * back and forward beside it.
 */
export class ReplayRecorder implements DrawingSink {
  private readonly recorder: ActionRecorder;
  /** The canvas the replay is of, read when the file is asked for. */
  private readonly size: () => { width: number; height: number };
  /** Whether the stroke in progress has opened its frame yet. */
  private frameOpen = false;
  /** The two-tone background flood the drawing opened with: 0 or 1. */
  private openingFills = 0;

  constructor(recordReplay: boolean, size: () => { width: number; height: number }) {
    this.recorder = new ActionRecorder(recordReplay);
    this.size = size;
  }

  pointerDown(): void {
    this.frameOpen = false;
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
    if (!this.frameOpen) {
      // A stroke normally opens with a dot, which writes the header with the
      // press point duplicated. Reaching here means the header is being
      // opened by a segment instead, which NEO's freeHandMove records as
      // (previous, new) so the replay draws that first segment.
      this.recorder.step();
      this.frameOpen = true;
      this.recorder.push(
        "freeHand",
        layerIndex(layer),
        color.r, color.g, color.b, color.a,
        mask.r, mask.g, mask.b,
        brushSize,
        mask.type,
        lineTypeForBrush(brushType),
        from.x, from.y, to.x, to.y,
      );
      return;
    }
    this.recorder.push(to.x, to.y);
  }

  dot(
    at: Point,
    brushSize: number,
    brushType: BrushType,
    color: Rgba,
    layer: Layer,
    mask: Mask,
  ): void {
    if (!this.frameOpen) {
      this.recorder.step();
      this.frameOpen = true;
    }
    this.recorder.push(
      "freeHand",
      layerIndex(layer),
      color.r, color.g, color.b, color.a,
      mask.r, mask.g, mask.b,
      brushSize,
      mask.type,
      lineTypeForBrush(brushType),
      at.x, at.y, at.x, at.y,
    );
  }

  fill(at: Point, color: Rgba, layer: Layer): boolean {
    // ABGR format: (alpha << 24) | (blue << 16) | (green << 8) | red
    const packed = (color.a << 24) | (color.b << 16) | (color.g << 8) | color.r;
    if (!this.frameOpen) {
      this.recorder.step();
      this.frameOpen = true;
    }
    // The replay file keeps the flood as a flood: it is one participant's
    // own drawing, replayed against the canvas it was made on, so the seed
    // reproduces it exactly and costs four numbers instead of a raster.
    this.recorder.push("floodFill", layerIndex(layer), at.x, at.y, packed);
    return false;
  }

  /** NEO records a cleared layer as ["eraseAll", layer]. */
  eraseAll(layer: Layer): void {
    this.recorder.step();
    this.recorder.push("eraseAll", layerIndex(layer));
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
    this.recorder.pushLine(
      layerIndex(layer), lineTypeForBrush(brushType), from, to, color, brushSize, mask,
    );
  }

  bezier(
    points: number[],
    brushSize: number,
    brushType: BrushType,
    color: Rgba,
    layer: Layer,
    mask: Mask,
  ): void {
    this.recorder.pushBezier(
      layerIndex(layer), lineTypeForBrush(brushType), points, color, brushSize, mask,
    );
  }

  /**
   * A region tool was applied; record it as its own frame. The verb and
   * whether it carries the drawing state come from the tool table, since
   * getting that boundary wrong shifts every field after it.
   */
  region(
    tool: RegionTool,
    layer: Layer,
    rect: RegionRect,
    color: Rgba,
    brushSize: number,
    mask: Mask,
  ): void {
    const shape = frameShapeFor(tool);
    if (!shape) return;
    const fillType = fillToolTypeFor(tool);
    this.recorder.pushRegion(
      shape.verb,
      shape.carriesDrawingState,
      layerIndex(layer),
      rect,
      color,
      brushSize,
      // A `fill` frame ends with which shape it is: NEO's fill action reads
      // it from item[15] and hands it to doFill, whose getMaskFunc draws
      // nothing for a type it does not know. This was left off, so every
      // rectangle and ellipse drawn here replayed as blank -- in NEO and in
      // our own viewer alike -- while the canvas it was recorded on showed it.
      fillType !== null ? [fillType] : [],
      mask,
    );
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
    const shape = frameShapeFor("paste");
    if (!shape) return;
    this.recorder.pushRegion(
      shape.verb,
      shape.carriesDrawingState,
      layerIndex(layer),
      source,
      color,
      brushSize,
      [dx, dy],
      mask,
    );
  }

  pointerUp(): void {
    this.frameOpen = false;
  }

  /** The recorder's head, moved with the snapshot stack. */
  undo(): void {
    this.recorder.back();
  }

  redo(): void {
    this.recorder.forward();
  }

  /** NEO's frame: ["text", layer, x, y, color, alpha, string, size, family] */
  recordText(
    layer: Layer,
    x: number,
    y: number,
    packedColor: number,
    alpha: number,
    text: string,
    fontSize: string,
    fontFamily: string,
  ): void {
    this.recorder.step();
    this.recorder.push("text", layerIndex(layer), x, y, packedColor, alpha, text, fontSize, fontFamily);
  }

  /**
   * A restore frame of both layers, closing the file. Written from the
   * buffers rather than the canvases on screen, so it holds exactly what was
   * drawn -- which is what a resumed drawing carries on from (see
   * `layerDataUrl`) -- and nothing a repaint has yet to catch up with.
   */
  addRestoreAction(engine: DrawingEngine): void {
    const { imageWidth: width, imageHeight: height } = engine;
    this.recorder.addRestoreAction(
      layerToPngDataUrl(engine.layers.background, width, height),
      layerToPngDataUrl(engine.layers.foreground, width, height),
    );
  }

  /** A restore frame the drawing opened with, which is not a stroke. */
  recordOpeningRestore(backgroundDataUrl: string, foregroundDataUrl: string): void {
    this.recorder.step();
    this.recorder.push("restore", backgroundDataUrl, foregroundDataUrl);
  }

  /** A flood the drawing opened with, which is not a stroke. */
  recordOpeningFill(packedColor: number): void {
    this.recorder.step();
    this.recorder.push("floodFill", 0, 0, 0, packedColor);
    this.openingFills += 1;
  }

  /**
   * Carry on recording after the frames of a `.pch` this painter saved.
   *
   * The two-tone flood this mount opened with is replaced along with
   * everything else, and the file's own -- its first frame, if it has one --
   * counts in its place.
   */
  resumeFrom(items: ActionItem[][]): void {
    const first = items[0];
    const opensWithFill =
      first !== undefined &&
      first[0] === "floodFill" &&
      first[1] === 0 &&
      first[2] === 0 &&
      first[3] === 0;
    this.openingFills = this.openingFills > 0 && opensWithFill ? 1 : 0;
    this.recorder.load(items);
  }

  getReplayBlob(): Blob {
    const { width, height } = this.size();
    return this.recorder.getReplayBlob(width, height);
  }

  /** Whether a replay is being kept at all. */
  get isRecording(): boolean {
    return this.recorder.isRecording;
  }

  actionCount(): number {
    return this.recorder.getActionCount();
  }

  /**
   * The strokes in the replay: every frame but the restores -- the image a
   * drawing was opened on, the end of each save -- and the two-tone flood
   * it opened with.
   */
  strokeCount(): number {
    return Math.max(
      0,
      this.recorder.getActionCount() -
        this.recorder.countWithinHead("restore") -
        this.openingFills,
    );
  }
}
