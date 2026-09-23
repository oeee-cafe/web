/**
 * Dragging a floating window.
 *
 * NEO docks its toolbox to the canvas and never moves it, so every window
 * this codebase floats is ours rather than a reproduction -- and the host's
 * panels float beside the painter's own. The gesture lives here, in one
 * framework-neutral function, because the last time two things in this
 * repository each had their own drag handling only one of them ever got
 * fixed.
 *
 * While the pointer is down it moves the frame itself and reports only where
 * the gesture ended. Reporting every step used to put each one through the
 * host's state: React files an update from a native `pointermove` as
 * continuous input and renders it in a task of its own, after the frame the
 * event arrived in has usually been painted, so the window trailed the
 * pointer by a frame and not always the same one. Safari runs pages at 60fps
 * even on a 120Hz screen, which makes each of those misses 16ms long, and
 * that is where the drag was seen to stutter. Moving the frame in the event
 * handler keeps it under the pointer on the frame the pointer moved.
 */

export interface WindowPosition {
  x: number;
  y: number;
}

export interface WindowDragOptions {
  /**
   * Called with the frame's viewport position when a drag ends. During the
   * drag the frame is moved by a `transform`, which is removed as the final
   * position is written to its `left` and `top` and handed here.
   */
  onPosition(position: WindowPosition): void;
  /**
   * How high the window may go, in viewport coordinates.
   *
   * The top of the painter's own area, so a window cannot be parked over the
   * chrome a host draws above it -- on the collaborative page that chrome is
   * the session title, the share button, the connection indicator and the only
   * way back to the lobby, and a panel dropped on it hides all four.
   */
  minimumY?: number;
}

/**
 * The area a window has to stay reachable inside.
 *
 * Two viewports disagree here and each is right about something. `window.inner*`
 * counts the strip underneath mobile Safari's URL bar, so clamping against it
 * can park a window just below the fold with no way left to grab it;
 * `visualViewport` reports what is genuinely on screen and fixes that.
 *
 * But pinch zoom shrinks the visual viewport while leaving the layout viewport
 * exactly as it was -- and a `position: fixed` window is placed against the
 * layout viewport. Clamping a fixed window to the visual one while zoomed in
 * dragged every panel inward to the edge of the magnified region, which is
 * nowhere near the edge of the screen it is pinned to. So the visual viewport
 * is used only at scale 1, where it differs from the layout viewport by the
 * browser's own chrome and nothing else.
 */
export function windowBounds(
  visual: Pick<VisualViewport, "width" | "height" | "scale"> | null =
    window.visualViewport,
): { width: number; height: number } {
  if (visual && Math.abs(visual.scale - 1) < 0.01) {
    return { width: visual.width, height: visual.height };
  }
  const layout = document.documentElement;
  return {
    width: layout.clientWidth || window.innerWidth,
    height: layout.clientHeight || window.innerHeight,
  };
}

/**
 * Hold a coordinate between the two limits on its axis, whichever way round
 * they are.
 *
 * They invert when a window is bigger than the space it is in: the far limit
 * (viewport minus window) falls below the near one, and the only way to see the
 * window's bottom is to push its top off the screen. Ordering the pair rather
 * than assuming one is the minimum is what makes that possible instead of
 * pinning the window at the top with its tail out of reach.
 */
function within(value: number, a: number, b: number): number {
  return Math.min(Math.max(value, Math.min(a, b)), Math.max(a, b));
}

/**
 * Make `handle` drag `frame`, and return the function that undoes it.
 *
 * One pointer gesture rather than a mouse pair and a touch pair. Pointer
 * capture keeps the window following even when the cursor outruns it, which
 * is what a document-level listener would otherwise work around. Positions are
 * clamped into the window, and no higher than `minimumY`.
 */
export function attachWindowDrag(
  frame: HTMLElement,
  handle: HTMLElement,
  options: WindowDragOptions,
): () => void {
  let offset: WindowPosition | null = null;
  /** Where the drag has put the frame, which is only its `transform` until it ends. */
  let target: WindowPosition | null = null;
  /** The translation currently applied, so the frame's own place can be recovered. */
  let shift: WindowPosition = { x: 0, y: 0 };

  const onPointerDown = (event: PointerEvent) => {
    const rect = frame.getBoundingClientRect();
    offset = { x: event.clientX - rect.left, y: event.clientY - rect.top };
    target = null;
    handle.setPointerCapture(event.pointerId);
    event.preventDefault();
  };

  const onPointerMove = (event: PointerEvent) => {
    if (!offset) return;
    const rect = frame.getBoundingClientRect();
    const bounds = windowBounds();
    target = {
      x: within(event.clientX - offset.x, 0, bounds.width - rect.width),
      y: within(
        event.clientY - offset.y,
        options.minimumY ?? 0,
        bounds.height - rect.height,
      ),
    };
    // Measured from where the frame sits under the translation rather than
    // from where the drag began, so a host that moves the window mid-drag --
    // clamping it to a viewport that just shrank -- does not throw it off.
    const base = { x: rect.left - shift.x, y: rect.top - shift.y };
    shift = { x: target.x - base.x, y: target.y - base.y };
    frame.style.transform = `translate(${shift.x}px, ${shift.y}px)`;
  };

  const endDrag = () => {
    if (!offset) return;
    offset = null;
    const end = target;
    target = null;
    shift = { x: 0, y: 0 };
    frame.style.transform = "";
    if (!end) return;
    // Written here as well as reported, so the frame does not spend a frame
    // back where the drag started while the host's state catches up.
    frame.style.left = `${end.x}px`;
    frame.style.top = `${end.y}px`;
    options.onPosition(end);
  };

  handle.addEventListener("pointerdown", onPointerDown);
  handle.addEventListener("pointermove", onPointerMove);
  handle.addEventListener("pointerup", endDrag);
  handle.addEventListener("pointercancel", endDrag);
  handle.addEventListener("lostpointercapture", endDrag);

  return () => {
    endDrag();
    handle.removeEventListener("pointerdown", onPointerDown);
    handle.removeEventListener("pointermove", onPointerMove);
    handle.removeEventListener("pointerup", endDrag);
    handle.removeEventListener("pointercancel", endDrag);
    handle.removeEventListener("lostpointercapture", endDrag);
  };
}

export interface WindowSize {
  width: number;
  height: number;
}

export interface WindowResizeOptions {
  /**
   * Called with the frame's size when a resize ends. During it the size is
   * written straight to the frame's `style`, for the reason the drag is.
   */
  onSize(size: WindowSize): void;
  /** How small the window may be made. */
  minimum?: WindowSize;
}

/**
 * Make `handle` resize `frame` from its bottom-right corner, and return the
 * function that undoes it.
 *
 * A window is resized by an element of ours rather than by CSS `resize`, which
 * draws no grabber at all on a touch browser -- on Chrome for Android a window
 * using it simply looks unresizable. The gesture is here beside the drag for
 * the same reason the drag is here: two windows, one implementation.
 *
 * The size is written to the frame as the corner moves and reported once, when
 * it is let go -- the drag above says why. The anchor is the corner the window
 * is pinned by, so a resize never moves it.
 *
 * Where inside the handle the gesture started is remembered, because the handle
 * is 20px of grabbable corner and almost nobody lands on its last pixel.
 * Sizing to the pointer alone snapped the window's corner under the finger the
 * instant it moved -- a jump of however far off the corner you happened to
 * press, before the resize proper had begun.
 */
export function attachWindowResize(
  frame: HTMLElement,
  handle: HTMLElement,
  options: WindowResizeOptions,
): () => void {
  let origin: {
    left: number;
    top: number;
    /** How far the frame's corner sits beyond the pointer that grabbed it. */
    offsetX: number;
    offsetY: number;
  } | null = null;
  /** The size the gesture has reached, reported when it ends. */
  let size: WindowSize | null = null;

  const onPointerDown = (event: PointerEvent) => {
    const rect = frame.getBoundingClientRect();
    size = null;
    origin = {
      left: rect.left,
      top: rect.top,
      offsetX: rect.right - event.clientX,
      offsetY: rect.bottom - event.clientY,
    };
    handle.setPointerCapture(event.pointerId);
    event.preventDefault();
    event.stopPropagation();
  };

  const onPointerMove = (event: PointerEvent) => {
    if (!origin) return;
    const bounds = windowBounds();
    const corner = {
      x: event.clientX + origin.offsetX,
      y: event.clientY + origin.offsetY,
    };
    size = {
      width: Math.max(
        options.minimum?.width ?? 0,
        Math.min(corner.x - origin.left, bounds.width - origin.left),
      ),
      height: Math.max(
        options.minimum?.height ?? 0,
        Math.min(corner.y - origin.top, bounds.height - origin.top),
      ),
    };
    frame.style.width = `${size.width}px`;
    frame.style.height = `${size.height}px`;
  };

  const endResize = () => {
    if (!origin) return;
    origin = null;
    const end = size;
    size = null;
    if (end) options.onSize(end);
  };

  handle.addEventListener("pointerdown", onPointerDown);
  handle.addEventListener("pointermove", onPointerMove);
  handle.addEventListener("pointerup", endResize);
  handle.addEventListener("pointercancel", endResize);
  handle.addEventListener("lostpointercapture", endResize);

  return () => {
    endResize();
    handle.removeEventListener("pointerdown", onPointerDown);
    handle.removeEventListener("pointermove", onPointerMove);
    handle.removeEventListener("pointerup", endResize);
    handle.removeEventListener("pointercancel", endResize);
    handle.removeEventListener("lostpointercapture", endResize);
  };
}

/**
 * Keep a window reachable when the viewport shrinks under it. Returns the
 * position it should sit at, which is the one it already has when it is
 * reachable where it stands.
 */
export function clampWindowPosition(
  position: WindowPosition,
  frame: HTMLElement,
  minimumY = 0,
): WindowPosition {
  const rect = frame.getBoundingClientRect();
  const bounds = windowBounds();
  return {
    x: within(position.x, 0, bounds.width - rect.width),
    y: within(position.y, minimumY, bounds.height - rect.height),
  };
}
