/**
 * The bookkeeping behind a two-finger pinch, kept away from the DOM so the
 * arithmetic can be checked without a touchscreen.
 *
 * Only two fingers ever drive the view, and they are the first two down. A
 * third joining is remembered but ignored, so a palm landing mid-pinch does
 * not throw the zoom across the room; it takes over only if one of the
 * original pair lifts while it is still there, and the gesture is re-seeded
 * from where the new pair happens to be rather than jumping to match the
 * separation the old one had.
 */

export interface PinchPoint {
  x: number;
  y: number;
}

/** Where one moved finger says the view should now be. */
export interface PinchSample {
  /** The pair's separation now, over their separation when it was seeded. */
  scale: number;
  /** The point between the two fingers, in client pixels. */
  center: PinchPoint;
  /** How far that midpoint has travelled since the previous sample. */
  panX: number;
  panY: number;
}

/**
 * What a press or a release did to the gesture.
 *
 * `seeded` is the one a caller has to act on: it means the pair -- and so the
 * separation everything is measured against -- is new, and whatever zoom the
 * view is at right now is the zoom this stretch of the gesture starts from.
 */
export type PinchChange = "none" | "seeded" | "ended";

const distanceBetween = (a: PinchPoint, b: PinchPoint) =>
  Math.hypot(b.x - a.x, b.y - a.y);

const midpointOf = (a: PinchPoint, b: PinchPoint): PinchPoint => ({
  x: (a.x + b.x) / 2,
  y: (a.y + b.y) / 2,
});

export class PinchGesture {
  /** Every finger down, in the order it arrived. */
  private readonly points = new Map<number, PinchPoint>();
  /** The two that count, or null while fewer than two are down. */
  private pair: [number, number] | null = null;
  private originDistance = 0;
  private lastCenter: PinchPoint | null = null;

  /** How many fingers are down, pair or not. */
  get pointerCount(): number {
    return this.points.size;
  }

  /** The fingers down, in the order they arrived. */
  get pointerIds(): number[] {
    return [...this.points.keys()];
  }

  /** Whether two fingers are driving the view right now. */
  get active(): boolean {
    return this.pair !== null;
  }

  down(pointerId: number, point: PinchPoint): PinchChange {
    this.points.set(pointerId, point);
    return this.pair ? "none" : this.seed();
  }

  move(pointerId: number, point: PinchPoint): PinchSample | null {
    if (!this.points.has(pointerId)) return null;
    this.points.set(pointerId, point);

    const pair = this.pair;
    if (!pair) return null;
    if (pointerId !== pair[0] && pointerId !== pair[1]) return null;

    const a = this.points.get(pair[0]);
    const b = this.points.get(pair[1]);
    if (!a || !b) return null;

    const center = midpointOf(a, b);
    const previous = this.lastCenter ?? center;
    this.lastCenter = center;

    return {
      // Two fingers landing on the same pixel have no separation to divide
      // by, and no opinion about scale either.
      scale:
        this.originDistance > 0
          ? distanceBetween(a, b) / this.originDistance
          : 1,
      center,
      panX: center.x - previous.x,
      panY: center.y - previous.y,
    };
  }

  up(pointerId: number): PinchChange {
    const wasDown = this.points.delete(pointerId);
    const pair = this.pair;
    if (!wasDown || !pair) return "none";
    if (pointerId !== pair[0] && pointerId !== pair[1]) return "none";

    this.pair = null;
    this.lastCenter = null;
    return this.points.size >= 2 ? this.seed() : "ended";
  }

  /** Forget every finger, as when the whole interaction is torn down. */
  clear(): void {
    this.points.clear();
    this.pair = null;
    this.originDistance = 0;
    this.lastCenter = null;
  }

  private seed(): PinchChange {
    if (this.points.size < 2) return "none";
    const [first, second] = [...this.points.keys()].slice(0, 2);
    const a = this.points.get(first);
    const b = this.points.get(second);
    if (!a || !b) return "none";

    this.pair = [first, second];
    this.originDistance = distanceBetween(a, b);
    this.lastCenter = midpointOf(a, b);
    return "seeded";
  }
}
