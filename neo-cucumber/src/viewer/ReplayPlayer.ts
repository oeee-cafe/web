import { NeoReplay } from "../neo/NeoReplay";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Frame = any[];

/**
 * Steps per second, not milliseconds per step. Driving playback from a rate is
 * what lets the labels mean what they say: browsers clamp setTimeout to about
 * 4ms, so one-step-per-timer tops out near 250 steps/s and the faster settings
 * would all collapse into each other.
 *
 * ×1 is 800 steps/s, which plays the median archived drawing (7,575 steps) in
 * about ten seconds.
 *
 * Slowest first, so the faster setting is the one further along, as it is on
 * the seek bar above them.
 */
export const SPEEDS = [
  { label: "×½", rate: 400 },
  { label: "×1", rate: 800 },
  { label: "×2", rate: 1600 },
  { label: "×4", rate: 3200 },
] as const;

export const DEFAULT_SPEED_INDEX = 1;

/** Ignore gaps longer than this, so returning to a background tab is not a jump. */
const MAX_FRAME_SECONDS = 0.1;

export interface PlayerState {
  position: number;
  total: number;
  playing: boolean;
  finished: boolean;
}

/**
 * Drives a NeoReplay forward one step at a time onto a visible canvas.
 *
 * Stepping backwards means replaying from the start, which is cheap -- a whole
 * archived drawing renders in a fraction of a second -- and avoids keeping
 * snapshots around.
 */
export class ReplayPlayer {
  readonly total: number;

  private readonly steps: { frame: number; step: number }[] = [];
  private replay: NeoReplay;
  private position = 0;
  private playing = false;
  private timer: number | null = null;
  private rate: number = SPEEDS[DEFAULT_SPEED_INDEX].rate;
  private carry = 0;
  private lastFrame = 0;
  private disposed = false;

  private readonly items: Frame[];
  /**
   * The replay's final image, when the file ends with one. Strokes render
   * through a canvas, which loses a little translucent colour that the
   * painter's buffers keep, so playback finishes by applying this and landing
   * on exactly the artwork rather than near it.
   */
  private readonly finalRestore: Frame | null;
  private restoreApplied = false;
  private readonly width: number;
  private readonly height: number;
  private readonly display: CanvasRenderingContext2D;
  private readonly onChange: (state: PlayerState) => void;

  constructor(
    items: Frame[],
    width: number,
    height: number,
    display: CanvasRenderingContext2D,
    onChange: (state: PlayerState) => void
  ) {
    this.width = width;
    this.height = height;
    this.display = display;
    this.onChange = onChange;

    // Only the restore a file ends on holds the finished drawing, and it is
    // kept back to settle on rather than played, so the strokes are what you
    // watch. Every earlier restore is the canvas the artist was already working
    // over -- a drawing continued from an earlier session opens with one -- and
    // plays in place, which is what NEO's own play() does with it. Dropping
    // those left a tenth of the archive animating over a blank canvas and only
    // showing the artwork on the last step.
    const last = items.length > 0 ? items[items.length - 1] : null;
    const endsOnRestore = last != null && last[0] === "restore";
    this.finalRestore = endsOnRestore ? last : null;
    this.items = endsOnRestore ? items.slice(0, -1) : items;

    for (let f = 0; f < this.items.length; f++) {
      const count = NeoReplay.stepsFor(this.items[f]);
      for (let s = 0; s < count; s++) this.steps.push({ frame: f, step: s });
    }
    this.total = this.steps.length;

    this.replay = new NeoReplay(width, height);
    this.paint();
  }

  getState(): PlayerState {
    return {
      position: this.position,
      total: this.total,
      playing: this.playing,
      finished: this.position >= this.total,
    };
  }

  private notify(): void {
    if (!this.disposed) this.onChange(this.getState());
  }

  /** White ground, then background layer, then foreground, as NEO composites. */
  private paint(): void {
    const { width, height } = this;
    this.display.clearRect(0, 0, width, height);
    this.display.fillStyle = "#ffffff";
    this.display.fillRect(0, 0, width, height);
    this.display.drawImage(this.replay.painter.canvas[0], 0, 0);
    this.display.drawImage(this.replay.painter.canvas[1], 0, 0);
  }

  /**
   * Applies the final image once playback reaches the end. A replay whose
   * restore frame will not load stays on the strokes rather than losing them.
   */
  private async settle(): Promise<void> {
    if (this.restoreApplied || !this.finalRestore) return;
    if (this.position < this.total) return;
    this.restoreApplied = true;
    try {
      await this.replay.apply(this.finalRestore);
      this.paint();
    } catch {
      // Damaged final image; what was drawn is better than nothing
    }
  }

  private async advance(): Promise<boolean> {
    if (this.position >= this.total) return false;
    const { frame, step } = this.steps[this.position];
    try {
      await this.replay.applyStep(this.items[frame], step);
    } catch {
      // A restore whose image will not load. Skipping it costs that frame's
      // pixels; letting it throw would strand playback on it entirely.
    }
    this.position++;
    return true;
  }

  private schedule(): void {
    if (this.timer !== null) cancelAnimationFrame(this.timer);
    this.lastFrame = performance.now();
    this.timer = requestAnimationFrame((now) => void this.tick(now));
  }

  private async tick(now: number): Promise<void> {
    if (!this.playing || this.disposed) return;

    const elapsed = Math.min((now - this.lastFrame) / 1000, MAX_FRAME_SECONDS);
    this.lastFrame = now;

    // Carry the fraction of a step across frames so the rate holds regardless
    // of how often the browser calls us back.
    this.carry += elapsed * this.rate;
    const due = Math.floor(this.carry);
    this.carry -= due;

    let drew = false;
    for (let i = 0; i < due; i++) {
      if (!(await this.advance())) break;
      drew = true;
    }

    if (drew) this.paint();

    if (this.position >= this.total) {
      this.playing = false;
      await this.settle();
      this.notify();
      return;
    }
    if (drew) this.notify();

    this.timer = requestAnimationFrame((next) => void this.tick(next));
  }

  play(): void {
    if (this.disposed || this.playing) return;
    if (this.position >= this.total) {
      void this.seekTo(0).then(() => {
        this.playing = true;
        this.notify();
        this.schedule();
      });
      return;
    }
    this.playing = true;
    this.notify();
    this.schedule();
  }

  pause(): void {
    this.playing = false;
    if (this.timer !== null) {
      cancelAnimationFrame(this.timer);
      this.timer = null;
    }
    this.carry = 0;
    this.notify();
  }

  /** Steps per second. */
  setSpeed(rate: number): void {
    this.rate = rate;
    this.carry = 0;
  }

  /** Jumps to a step, replaying from the start when moving backwards. */
  async seekTo(target: number): Promise<void> {
    const clamped = Math.max(0, Math.min(this.total, Math.round(target)));

    if (clamped < this.position) {
      this.replay = new NeoReplay(this.width, this.height);
      this.position = 0;
      this.restoreApplied = false;
    }
    while (this.position < clamped) {
      if (!(await this.advance())) break;
    }
    this.paint();
    await this.settle();
    this.notify();
  }

  async rewind(): Promise<void> {
    this.pause();
    await this.seekTo(0);
  }

  /** Draws the whole thing immediately, skipping the animation. */
  async skipToEnd(): Promise<void> {
    this.pause();
    await this.seekTo(this.total);
  }

  dispose(): void {
    this.disposed = true;
    this.playing = false;
    if (this.timer !== null) cancelAnimationFrame(this.timer);
  }
}
