import { afterEach, describe, expect, it } from "vitest";
import { attachWindowResize, type WindowSize } from "./windowDrag";

/**
 * Resizing a floating window by its corner.
 *
 * The corner is an element of ours because CSS `resize` draws no grabber on a
 * touch browser -- on Chrome for Android there was nothing there to find. What
 * matters here is that the gesture works from a touch pointer at all, which is
 * what the thing it replaced did not do.
 */

let cleanup: (() => void) | null = null;

function mount() {
  const frame = document.createElement("div");
  frame.style.cssText = "position:fixed;left:40px;top:20px;width:224px;height:200px";
  const corner = document.createElement("div");
  frame.appendChild(corner);
  document.body.appendChild(frame);
  corner.setPointerCapture = () => {};

  const sizes: WindowSize[] = [];
  const detach = attachWindowResize(frame, corner, {
    minimum: { width: 180, height: 140 },
    onSize: (size) => sizes.push(size),
  });
  cleanup = () => {
    detach();
    frame.remove();
  };
  return { frame, corner, sizes };
}

/** The size the frame has on screen, which is what the gesture changes as it goes. */
function shown(frame: HTMLElement): WindowSize {
  const rect = frame.getBoundingClientRect();
  return { width: rect.width, height: rect.height };
}

function pointer(type: string, x: number, y: number): PointerEvent {
  return new PointerEvent(type, {
    pointerId: 3,
    pointerType: "touch",
    button: type === "pointermove" ? -1 : 0,
    buttons: type === "pointerup" ? 0 : 1,
    clientX: x,
    clientY: y,
    bubbles: true,
  });
}

afterEach(() => {
  cleanup?.();
  cleanup = null;
});

describe("resizing a floating window", () => {
  it("does not jump when the corner is grabbed away from its last pixel", () => {
    const { frame, corner, sizes } = mount();

    // 8px up and to the left of the frame's corner at (264, 220), which is
    // where a finger on a 20px handle actually lands.
    corner.dispatchEvent(pointer("pointerdown", 256, 212));
    corner.dispatchEvent(pointer("pointermove", 256, 212));

    // Nothing moved, so nothing resized.
    expect(shown(frame)).toEqual({ width: 224, height: 200 });

    // And from there the window follows the pointer one-for-one.
    corner.dispatchEvent(pointer("pointermove", 296, 262));
    expect(shown(frame)).toEqual({ width: 264, height: 250 });

    // Reported once, when it is let go, rather than a step behind all along.
    expect(sizes).toEqual([]);
    corner.dispatchEvent(pointer("pointerup", 296, 262));
    expect(sizes).toEqual([{ width: 264, height: 250 }]);
  });

  it("sizes it to the corner under a touch pointer", () => {
    const { corner, sizes } = mount();

    corner.dispatchEvent(pointer("pointerdown", 264, 220));
    corner.dispatchEvent(pointer("pointermove", 340, 400));
    corner.dispatchEvent(pointer("pointerup", 340, 400));

    // The frame is pinned at (40, 20), so the corner's position is its size.
    expect(sizes.at(-1)).toEqual({ width: 300, height: 380 });
  });

  it("refuses to shrink below the minimum", () => {
    const { frame, corner, sizes } = mount();

    corner.dispatchEvent(pointer("pointerdown", 264, 220));
    corner.dispatchEvent(pointer("pointermove", 60, 40));

    expect(shown(frame)).toEqual({ width: 180, height: 140 });
    corner.dispatchEvent(pointer("pointerup", 60, 40));
    expect(sizes.at(-1)).toEqual({ width: 180, height: 140 });
  });

  it("does not move the window it is sizing", () => {
    const { frame, corner } = mount();

    corner.dispatchEvent(pointer("pointerdown", 264, 220));
    corner.dispatchEvent(pointer("pointermove", 340, 400));

    expect(frame.style.left).toBe("40px");
    expect(frame.style.top).toBe("20px");
  });
});
