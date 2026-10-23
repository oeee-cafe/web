import { compressToUint8Array } from "lz-string";
import { NO_MASK, type Mask } from "../neo/mask";
import { AFTER_DRAWING_STATE, DRAWING_STATE_AT } from "../neo/pchFrames";

// Type for action items - can be strings, numbers, or nested arrays
export type ActionItem = string | number | ActionItem[];

/**
 * The mask a stroke was drawn through. Every verb that carries the drawing
 * state has four slots for it, so it travels with the stroke rather than
 * being replayed from whatever the UI happens to hold later.
 */
export type { Mask as MaskState } from "../neo/mask";
export { NO_MASK } from "../neo/mask";

/**
 * ActionRecorder - Records drawing operations for replay
 * Mirrors Neo.ActionManager functionality
 */
export class ActionRecorder {
  private items: ActionItem[][] = [];
  private head: number = 0;

  /**
   * Whether anything is being recorded at all.
   *
   * A collaborative session has no use for a replay file: it saves a flattened
   * image, and the format could not describe the session anyway -- `.pch`
   * addresses two layers and a session has a pair per participant. Recording
   * one regardless costs a growing list of actions and, at every restore
   * point, two full-canvas PNGs held in memory for nothing.
   *
   * The switch is here rather than at the call sites because there are a dozen
   * of those and they only have to be right once each to be wrong forever.
   */
  private readonly recording: boolean;

  constructor(recording: boolean = true) {
    this.recording = recording;
  }

  /**
   * Create a new action frame (called on pointer down / stroke start)
   */
  step(): void {
    if (!this.recording) return;
    // Truncate items array if we're not at the end (redo history should be discarded)
    if (this.items.length > this.head) {
      this.items.length = this.head;
    }
    // Push empty array for new action
    this.items.push([]);
    this.head++;
  }

  /**
   * Add data to the current action frame
   */
  push(...args: ActionItem[]): void {
    if (!this.recording) return;
    if (this.head > 0 && this.head <= this.items.length) {
      const currentAction = this.items[this.head - 1];
      currentAction.push(...args);
    }
  }

  /**
   * Undo - move head back
   */
  back(): void {
    if (!this.recording) return;
    if (this.head > 0) {
      this.head--;
    }
  }

  /**
   * Redo - move head forward
   */
  forward(): void {
    if (!this.recording) return;
    if (this.head < this.items.length) {
      this.head++;
    }
  }

  /**
   * Add restore action at the end (final state of both layers)
   * This enables animation skip in Neo.Painter
   */
  addRestoreAction(bgDataURL: string, fgDataURL: string): void {
    if (!this.recording) return;
    // Discard undone actions first, exactly as step() does. Without this the
    // restore lands beyond the export window while head++ pulls an undone
    // action into it, so saving after an undo replays the wrong strokes and
    // ships no restore frame at all.
    if (this.items.length > this.head) {
      this.items.length = this.head;
    }

    // Check if last action is already a restore action
    const lastItem = this.items.length > 0 ? this.items[this.items.length - 1] : null;
    if (lastItem && lastItem[0] === "restore") {
      // Replace existing restore action
      this.items[this.items.length - 1] = ["restore", bgDataURL, fgDataURL];
    } else {
      // Add new restore action
      this.items.push(["restore", bgDataURL, fgDataURL]);
      this.head++;
    }
  }

  /**
   * Records a region tool as its own action.
   *
   * Verbs that carry the drawing state write pushCurrent's nine slots first --
   * colour, mask, width, mask type -- which is why their geometry starts at
   * `AFTER_DRAWING_STATE` rather than `DRAWING_STATE_AT`. Getting that
   * boundary wrong shifts every following field, so the shape comes from the
   * tool table rather than from each call site, and the offsets from
   * `pchFrames`, which is where the readers get them too.
   */
  pushRegion(
    verb: string,
    carriesDrawingState: boolean,
    layer: number,
    rect: { x: number; y: number; width: number; height: number },
    color: { r: number; g: number; b: number; a: number },
    brushSize: number,
    trailing: number[] = [],
    mask: Mask = NO_MASK
  ): void {
    this.step();
    const geometry = [rect.x, rect.y, rect.width, rect.height, ...trailing];
    const frame: ActionItem[] = carriesDrawingState
      ? [
          verb,
          layer,
          color.r, color.g, color.b, color.a,
          mask.r, mask.g, mask.b,
          brushSize,
          mask.type,
          ...geometry,
        ]
      : [verb, layer, ...geometry];
    // The geometry has to land where the readers look for it. One assertion
    // beats discovering it from a file somebody drew a year ago.
    const expected = carriesDrawingState
      ? AFTER_DRAWING_STATE
      : DRAWING_STATE_AT;
    if (frame.length - geometry.length !== expected) {
      throw new Error(
        `${verb} frame puts its geometry at ${frame.length - geometry.length}, not ${expected}`
      );
    }
    this.push(...frame);
  }

  /**
   * Records a straight line. NEO pushes the drawing state first, so lineType
   * lands at slot 11 and the endpoints at 12..15.
   */
  pushLine(
    layer: number,
    lineType: number,
    from: { x: number; y: number },
    to: { x: number; y: number },
    color: { r: number; g: number; b: number; a: number },
    brushSize: number,
    mask: Mask = NO_MASK
  ): void {
    this.step();
    this.push(
      "line", layer,
      color.r, color.g, color.b, color.a,
      mask.r, mask.g, mask.b,
      brushSize, mask.type,
      lineType,
      Math.round(from.x), Math.round(from.y),
      Math.round(to.x), Math.round(to.y)
    );
  }

  /**
   * Records a bezier. Like `line`, it pushes the drawing state first, so
   * lineType lands at slot 11 and the eight control coordinates at 12..19.
   */
  pushBezier(
    layer: number,
    lineType: number,
    points: number[],
    color: { r: number; g: number; b: number; a: number },
    brushSize: number,
    mask: Mask = NO_MASK
  ): void {
    this.step();
    this.push(
      "bezier", layer,
      color.r, color.g, color.b, color.a,
      mask.r, mask.g, mask.b,
      brushSize, mask.type,
      lineType,
      ...points.map((v) => Math.round(v))
    );
  }

  /**
   * Number of actions that will be exported, i.e. everything up to head.
   * Undone actions are excluded, so this is the honest basis for a stroke
   * count rather than a counter that only ever increments.
   */
  getActionCount(): number {
    return this.head;
  }

  /**
   * Carry on from the frames of a file this recorder once wrote, as NEO's
   * loadAnimation does: they become the start of this recording, and what is
   * drawn next is added after them. Nothing of them can be undone -- the
   * history a drawing is resumed with starts where they end.
   */
  load(items: ActionItem[][]): void {
    if (!this.recording) return;
    this.items = items.map((item) => item.slice());
    this.head = this.items.length;
  }

  /** How many frames up to the head have `verb` for their name. */
  countWithinHead(verb: string): number {
    let count = 0;
    for (let i = 0; i < this.head; i++) {
      if (this.items[i][0] === verb) count++;
    }
    return count;
  }

  /**
   * Generate replay blob in PCH format
   */
  /** True when this recorder is keeping a replay at all. */
  get isRecording(): boolean {
    return this.recording;
  }

  getReplayBlob(width: number, height: number): Blob {
    if (!this.recording) {
      throw new Error("This painter was mounted without replay recording");
    }
    // Truncate items to current head position
    const itemsToExport = this.items.slice(0, this.head);

    // Serialize to JSON and compress
    const data = JSON.stringify(itemsToExport);
    const compressedData = compressToUint8Array(data);

    // PCH format:
    // - Magic: "NEO " (4 bytes)
    // - Width: 2 bytes (little-endian)
    // - Height: 2 bytes (little-endian)
    // - Reserved: 4 bytes (zeros)
    // - Compressed data
    const magic = "NEO ";
    const widthBytes = new Uint8Array([width % 0x100, Math.floor(width / 0x100)]);
    const heightBytes = new Uint8Array([height % 0x100, Math.floor(height / 0x100)]);
    const reserved = new Uint8Array(4);

    return new Blob([
      magic,
      widthBytes,
      heightBytes,
      reserved,
      new Uint8Array(compressedData),
    ]);
  }
}
