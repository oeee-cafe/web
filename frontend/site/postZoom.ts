// A drawing on its post page, pinched closer inside its own frame.
//
// The apps hold the page still -- base.jinja keeps the phone and tablet
// web views from pinching, and theme_head.jinja turns the Mac and Windows
// apps' pinch away -- so the drawing does its own: two fingers, a trackpad's
// pinch (a ctrl-wheel in Chromium and WebView2, gesture events in WebKit),
// or a double tap. It never grows past its frame; once closer, a finger,
// the mouse or the wheel moves it about inside, and pinching back out, or
// another double tap, puts it as it was.
//
// When a pinch ends it settles on a whole multiple of the drawing's own
// pixels, as the painter zooms: a fractional step makes some pixels one
// screen pixel wide and some two. And from the moment a pixel of the drawing
// is larger than a pixel of the page it is drawn hard (is-crisp, style.css),
// as the replay's canvas is; smaller, it is scaled down smoothly.
//
// A piece of chrome.js, which post_view.jinja calls after the stage is
// fitted. No pointer events, which are Firefox 59: that is the API, not the
// syntax, and no build lowers it.

const MOST = 16; // drawing pixels to a page pixel, at the closest

// WebKit's trackpad pinch, which lib.dom does not describe.
interface GestureEvent extends UIEvent {
  scale: number;
  clientX: number;
  clientY: number;
}

interface Point {
  x: number;
  y: number;
}

function attach(stage: Element): void {
  const view = stage.querySelector<HTMLElement>("[data-post-zoom]");
  if (!view) return;
  const layer = view.firstElementChild as HTMLElement | null;
  const natural = parseInt(stage.getAttribute("data-width") || "", 10);
  if (!layer || !natural) return;

  let scale = 1;
  let x = 0;
  let y = 0;

  function shown(): number {
    return view!.clientWidth || natural;
  }
  // Drawing pixels to a page pixel at `s`.
  function pixels(s: number): number {
    return (shown() * s) / natural;
  }
  function largest(): number {
    return Math.max(1, (MOST * natural) / shown());
  }

  function clamp(s: number): number {
    return Math.max(1, Math.min(s, largest()));
  }
  // Kept covering the frame: no edge of the drawing comes inside it.
  function keepInside(): void {
    const width = view!.clientWidth;
    const height = view!.clientHeight;
    x = Math.min(0, Math.max(x, width - width * scale));
    y = Math.min(0, Math.max(y, height - height * scale));
  }
  function paint(settling: boolean): void {
    keepInside();
    view!.classList.toggle("is-settling", settling);
    view!.classList.toggle("is-closer", scale > 1);
    stage.classList.toggle("is-crisp", pixels(scale) >= 0.999);
    const transform = scale === 1 ? "" : "translate(" + x + "px," + y + "px) scale(" + scale + ")";
    layer!.style.webkitTransform = transform;
    layer!.style.transform = transform;
  }

  // Scale to `s`, keeping the point of the frame at (px, py) where it is.
  function zoomAt(s: number, px: number, py: number, settling: boolean): void {
    s = clamp(s);
    x = px - ((px - x) * s) / scale;
    y = py - ((py - y) * s) / scale;
    scale = s;
    paint(settling);
  }
  // The nearest whole step, once closer than the drawing's own size.
  function settle(px?: number, py?: number): void {
    const step = pixels(scale);
    if (step < 1) return paint(true);
    const whole = Math.max(1, Math.round(step));
    let s = scale === 1 ? 1 : (whole * natural) / shown();
    if (s < 1.001) s = 1;
    if (px === undefined || py === undefined) {
      px = view!.clientWidth / 2;
      py = view!.clientHeight / 2;
    }
    zoomAt(s, px, py, true);
  }
  function reset(): void {
    scale = 1;
    x = 0;
    y = 0;
    paint(true);
  }
  // A double tap or click: closer by a whole step or two, or back.
  function toggleAt(px: number, py: number): void {
    if (scale > 1) return reset();
    const step = Math.max(2, Math.floor(pixels(1)) * 2);
    zoomAt((step * natural) / shown(), px, py, true);
  }

  function local(clientX: number, clientY: number): Point {
    const box = view!.getBoundingClientRect();
    return { x: clientX - box.left, y: clientY - box.top };
  }

  // Touch: two fingers pinch, one moves the drawing once it is closer.
  let pinch: { d: number; scale: number; cx: number; cy: number } | null = null;
  let pan: Point | null = null;
  let lastTap: { time: number; x: number; y: number } | null = null;
  let moved = false;
  function between(touches: TouchList): Point & { d: number } {
    const a = local(touches[0].clientX, touches[0].clientY);
    const b = local(touches[1].clientX, touches[1].clientY);
    return {
      x: (a.x + b.x) / 2,
      y: (a.y + b.y) / 2,
      d: Math.sqrt((a.x - b.x) * (a.x - b.x) + (a.y - b.y) * (a.y - b.y)) || 1,
    };
  }
  function startPan(touch: Touch): void {
    const at = local(touch.clientX, touch.clientY);
    pan = { x: at.x - x, y: at.y - y };
  }
  view.addEventListener(
    "touchstart",
    (event) => {
      if (event.touches.length >= 2) {
        const mid = between(event.touches);
        pinch = { d: mid.d, scale, cx: (mid.x - x) / scale, cy: (mid.y - y) / scale };
        pan = null;
        moved = true;
        event.preventDefault();
      } else if (event.touches.length === 1) {
        moved = false;
        if (scale > 1) startPan(event.touches[0]);
      }
    },
    { passive: false },
  );
  view.addEventListener(
    "touchmove",
    (event) => {
      if (pinch && event.touches.length >= 2) {
        const mid = between(event.touches);
        scale = clamp((pinch.scale * mid.d) / pinch.d);
        x = mid.x - pinch.cx * scale;
        y = mid.y - pinch.cy * scale;
        paint(false);
        event.preventDefault();
      } else if (pan && event.touches.length === 1) {
        const at = local(event.touches[0].clientX, event.touches[0].clientY);
        x = at.x - pan.x;
        y = at.y - pan.y;
        moved = true;
        paint(false);
        event.preventDefault();
      } else {
        moved = true;
      }
    },
    { passive: false },
  );
  view.addEventListener("touchend", (event) => {
    if (pinch && event.touches.length < 2) {
      const last = pinch;
      pinch = null;
      settle(last.cx * scale + x, last.cy * scale + y);
      if (event.touches.length === 1 && scale > 1) startPan(event.touches[0]);
      return;
    }
    if (event.touches.length > 0) return;
    pan = null;
    if (moved || event.changedTouches.length !== 1) {
      lastTap = null;
      return;
    }
    const touch = event.changedTouches[0];
    const now = Date.now();
    if (
      lastTap &&
      now - lastTap.time < 300 &&
      Math.abs(touch.clientX - lastTap.x) < 30 &&
      Math.abs(touch.clientY - lastTap.y) < 30
    ) {
      lastTap = null;
      const at = local(touch.clientX, touch.clientY);
      toggleAt(at.x, at.y);
      // No click, nor the double click Chrome would make of it.
      event.preventDefault();
    } else {
      lastTap = { time: now, x: touch.clientX, y: touch.clientY };
    }
  });
  view.addEventListener("touchcancel", () => {
    if (pinch) settle();
    pinch = null;
    pan = null;
  });

  // A trackpad's pinch in WebKit. iOS sends these alongside the touches
  // above, which have it already.
  let gesture: { scale: number; x: number; y: number } | null = null;
  view.addEventListener("gesturestart", (event) => {
    const e = event as GestureEvent;
    e.preventDefault();
    if (pinch) return;
    const at = local(e.clientX, e.clientY);
    gesture = { scale, x: at.x, y: at.y };
  });
  view.addEventListener("gesturechange", (event) => {
    const e = event as GestureEvent;
    e.preventDefault();
    if (!gesture || pinch) return;
    zoomAt(gesture.scale * e.scale, gesture.x, gesture.y, false);
  });
  view.addEventListener("gestureend", (event) => {
    event.preventDefault();
    if (!gesture || pinch) return;
    const at = gesture;
    gesture = null;
    settle(at.x, at.y);
  });

  // A trackpad's pinch everywhere else, and Ctrl with the wheel; once
  // closer, the wheel moves the drawing, until it reaches an edge.
  let wheelEnd: ReturnType<typeof setTimeout> | undefined;
  view.addEventListener(
    "wheel",
    (event) => {
      const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? view.clientHeight : 1;
      const at = local(event.clientX, event.clientY);
      if (event.ctrlKey) {
        event.preventDefault();
        zoomAt(scale * Math.exp((-event.deltaY * unit) / 100), at.x, at.y, false);
        clearTimeout(wheelEnd);
        wheelEnd = setTimeout(() => {
          settle(at.x, at.y);
        }, 200);
        return;
      }
      if (scale === 1) return;
      const beforeX = x;
      const beforeY = y;
      x -= event.deltaX * unit;
      y -= event.deltaY * unit;
      paint(false);
      if (x !== beforeX || y !== beforeY) event.preventDefault();
    },
    { passive: false },
  );

  // The mouse moves the drawing once it is closer.
  let drag: Point | null = null;
  view.addEventListener("mousedown", (event) => {
    if (event.button !== 0 || scale === 1) return;
    drag = { x: event.clientX - x, y: event.clientY - y };
    view.classList.add("is-dragging");
    event.preventDefault();
  });
  window.addEventListener("mousemove", (event) => {
    if (!drag) return;
    x = event.clientX - drag.x;
    y = event.clientY - drag.y;
    paint(false);
  });
  window.addEventListener("mouseup", () => {
    if (!drag) return;
    drag = null;
    view.classList.remove("is-dragging");
  });
  view.addEventListener("dblclick", (event) => {
    const at = local(event.clientX, event.clientY);
    toggleAt(at.x, at.y);
    event.preventDefault();
  });
  view.addEventListener("dragstart", (event) => {
    event.preventDefault();
  });

  // A new size of window is a new frame: as it was, and sharp or not
  // for the size the page now shows it at.
  window.addEventListener("resize", reset);
  paint(false);
}

/** Every drawing's stage on the page, pinched closer in its own frame. */
export function postZoom(): void {
  const stages = document.querySelectorAll("[data-post-stage]");
  for (let i = 0; i < stages.length; i++) attach(stages[i]);
}
