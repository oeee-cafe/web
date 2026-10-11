import { useEffect, useRef } from "react";
import {
  MAX_BRUSH_SIZE,
  MIN_BRUSH_SIZE,
  SIZE_DRAG_PIXELS,
} from "../../constants/drawing";
import {
  NEO_BAR_LABEL,
  NEO_SIZE_SLIDER,
  NEO_SIZE_SLIDER_FILL,
  NEO_SIZE_SLIDER_HIT,
} from "./neoClasses";

// The bar is 33px tall and its hit area starts 4px above it. Both numbers are
// in NEO's drag arithmetic, which is why they are here and not only in CSS.
const BAR = 33;
const OFFSET = 4;
/**
 * Pixels of scrolling that make one step, for a device that scrolls in
 * pixels. A wheel's notch is far more than this and a trackpad's event far
 * less, so a notch is still one step and a trackpad has to travel for it.
 */
const WHEEL_STEP = 40;

const clamp = (v: number) =>
  Math.max(MIN_BRUSH_SIZE, Math.min(MAX_BRUSH_SIZE, Math.round(v)));

/** SizeSlider.update: bar height is the value scaled onto 33px. */
function heightFor(value: number): number {
  return Math.max(Math.min(34, (value * BAR) / MAX_BRUSH_SIZE), 1);
}

/**
 * NEO's brush size slider.
 *
 * The press is NEO's: it sets the size from the pointer's height, so a size
 * is one press away. What follows the press is not. NEO keeps reading the
 * height while the pointer stays in the box and only slows to one step per
 * seven pixels once it leaves, and the box gives thirty sizes 33 pixels --
 * about one each, which a mouse can just hit and a pen or a finger cannot.
 * Here the whole drag is the slow one, so the press lands near a size and
 * the drag walks to it. The formula is still SizeSlider.slide's, only
 * without its first branch; what a size draws is untouched, so nothing
 * recorded can tell.
 *
 * A wheel over the bar steps it by one, and so do `[` and `]` anywhere.
 */
export function NeoSizeSlider({
  value,
  color,
  onChange,
}: {
  value: number;
  color: string;
  onChange: (value: number) => void;
}) {
  const hitRef = useRef<HTMLDivElement>(null);
  const drag = useRef<{ value0: number; y0: number } | null>(null);

  const press = (y: number) => {
    // The pointer's height *is* the value, and the drag counts from there
    const value0 = clamp(Math.floor(((y - OFFSET) * MAX_BRUSH_SIZE) / BAR));
    drag.current = { value0, y0: y };
    onChange(value0);
  };

  const slide = (y: number) => {
    const state = drag.current;
    if (!state) return;
    onChange(clamp(state.value0 + (y - state.y0) / SIZE_DRAG_PIXELS));
  };

  // The wheel is bound by hand because React's `onWheel` is passive, and a
  // wheel that only changes the size must not scroll the page under it too.
  const latest = useRef({ value, onChange });
  latest.current = { value, onChange };
  useEffect(() => {
    const hit = hitRef.current;
    if (!hit) return;
    let travelled = 0;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      if (e.deltaY === 0) return;
      // Firefox reports a notch in lines; anything else arrives in pixels
      if (e.deltaMode === 0) {
        travelled += e.deltaY;
        if (Math.abs(travelled) < WHEEL_STEP) return;
      }
      travelled = 0;
      const { value, onChange } = latest.current;
      // Scrolling up makes it bigger, as `]` does
      onChange(clamp(value + (e.deltaY < 0 ? 1 : -1)));
    };
    hit.addEventListener("wheel", onWheel, { passive: false });
    return () => hit.removeEventListener("wheel", onWheel);
  }, []);

  // Measured against the hit area, whose origin is 4px above the bar -- which
  // is where the "y - 4" in NEO's formula comes from.
  const yFrom = (clientY: number) =>
    clientY - (hitRef.current?.getBoundingClientRect().top ?? 0);

  const stop = () => {
    drag.current = null;
  };

  return (
    <div className={NEO_SIZE_SLIDER}>
      <div
        className={NEO_SIZE_SLIDER_FILL}
        style={{
          height: `${heightFor(value).toFixed(2)}px`,
          backgroundColor: color,
        }}
      />
      <div className={NEO_BAR_LABEL}>{value}px</div>
      <div
        ref={hitRef}
        className={NEO_SIZE_SLIDER_HIT}
        style={{ touchAction: "none" }}
        role="slider"
        aria-label="Brush size"
        title="Brush size — press near a size and drag to fine-tune; the wheel and [ ] step it, and Ctrl+Alt+drag on the canvas sets it too"
        aria-valuemin={MIN_BRUSH_SIZE}
        aria-valuemax={MAX_BRUSH_SIZE}
        aria-valuenow={value}
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === "ArrowUp") onChange(clamp(value + 1));
          if (e.key === "ArrowDown") onChange(clamp(value - 1));
        }}
        onPointerDown={(e) => {
          e.preventDefault();
          const y = yFrom(e.clientY);
          if (e.shiftKey) {
            // Shift nudges by one, as SizeSlider.shift does
            onChange(clamp(value + (y < OFFSET + BAR / 2 ? -1 : 1)));
            return;
          }
          e.currentTarget.setPointerCapture(e.pointerId);
          press(y);
        }}
        onPointerMove={(e) => {
          if (drag.current) slide(yFrom(e.clientY));
        }}
        onPointerUp={stop}
        onPointerCancel={stop}
      />
    </div>
  );
}
