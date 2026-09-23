import { afterEach, describe, expect, it } from "vitest";
import { attachWindowDrag, windowBounds, type WindowPosition } from "./windowDrag";

/**
 * Dragging a floating window by its title bar.
 *
 * Synthetic PointerEvents are not in Chromium's active pointer table, so
 * `setPointerCapture` is stubbed out the way the slider tests do it; capture
 * itself is the browser's behaviour, not ours. What is checked here is the
 * arithmetic underneath it, which is this package's.
 */

interface Harness {
  frame: HTMLElement;
  handle: HTMLElement;
  positions: WindowPosition[];
  detach(): void;
}

let harness: Harness | null = null;

function mount(minimumY?: number, height = 200): Harness {
  const frame = document.createElement("div");
  frame.style.cssText = `position:fixed;left:0;top:0;width:200px;height:${height}px`;
  const handle = document.createElement("div");
  handle.style.cssText = "height:18px";
  frame.appendChild(handle);
  document.body.appendChild(frame);
  handle.setPointerCapture = () => {};

  const positions: WindowPosition[] = [];
  const detachDrag = attachWindowDrag(frame, handle, {
    minimumY,
    onPosition: (position) => positions.push(position),
  });

  harness = {
    frame,
    handle,
    positions,
    detach: () => {
      detachDrag();
      frame.remove();
    },
  };
  return harness;
}

function pointer(type: string, x: number, y: number): PointerEvent {
  return new PointerEvent(type, {
    pointerId: 7,
    pointerType: "touch",
    button: type === "pointermove" ? -1 : 0,
    buttons: type === "pointerup" ? 0 : 1,
    clientX: x,
    clientY: y,
    bubbles: true,
  });
}

function drag(handle: HTMLElement, from: WindowPosition, to: WindowPosition) {
  handle.dispatchEvent(pointer("pointerdown", from.x, from.y));
  handle.dispatchEvent(pointer("pointermove", to.x, to.y));
  handle.dispatchEvent(pointer("pointerup", to.x, to.y));
}

afterEach(() => {
  harness?.detach();
  harness = null;
});

describe("dragging a floating window", () => {
  it("moves it by the distance the pointer travelled", () => {
    const { handle, positions } = mount();

    drag(handle, { x: 10, y: 10 }, { x: 60, y: 90 });

    expect(positions.at(-1)).toEqual({ x: 50, y: 80 });
  });

  it("moves the frame itself mid-drag and reports only where it ended", () => {
    // Reporting every step put each one through React, which renders an
    // update from a native pointermove after the frame it arrived in -- a
    // window a frame behind the pointer, which Safari's 60fps made visible.
    const { frame, handle, positions } = mount();

    handle.dispatchEvent(pointer("pointerdown", 10, 10));
    handle.dispatchEvent(pointer("pointermove", 40, 50));
    handle.dispatchEvent(pointer("pointermove", 60, 90));

    expect(positions).toEqual([]);
    const moving = frame.getBoundingClientRect();
    expect({ x: moving.left, y: moving.top }).toEqual({ x: 50, y: 80 });

    handle.dispatchEvent(pointer("pointerup", 60, 90));

    expect(positions).toEqual([{ x: 50, y: 80 }]);
    expect(frame.style.transform).toBe("");
    const settled = frame.getBoundingClientRect();
    expect({ x: settled.left, y: settled.top }).toEqual({ x: 50, y: 80 });
  });

  it("follows a host that moves the window mid-drag", () => {
    const { frame, handle, positions } = mount();

    handle.dispatchEvent(pointer("pointerdown", 10, 10));
    handle.dispatchEvent(pointer("pointermove", 30, 30));
    frame.style.left = "5px";
    handle.dispatchEvent(pointer("pointermove", 60, 90));

    const rect = frame.getBoundingClientRect();
    expect({ x: rect.left, y: rect.top }).toEqual({ x: 50, y: 80 });

    handle.dispatchEvent(pointer("pointerup", 60, 90));
    expect(positions).toEqual([{ x: 50, y: 80 }]);
  });

  it("reports nothing for a press that never moved", () => {
    const { handle, positions } = mount();

    handle.dispatchEvent(pointer("pointerdown", 10, 10));
    handle.dispatchEvent(pointer("pointerup", 10, 10));

    expect(positions).toEqual([]);
  });

  it("keeps it on screen when dragged past the edge", () => {
    const { handle, positions } = mount();

    drag(handle, { x: 10, y: 10 }, { x: -400, y: -400 });

    expect(positions.at(-1)).toEqual({ x: 0, y: 0 });
  });

  it("stops at the chrome a host draws above the painter", () => {
    const { handle, positions } = mount(70);

    drag(handle, { x: 10, y: 100 }, { x: 10, y: -80 });

    expect(positions.at(-1)?.y).toBe(70);
  });

  it("lets a window taller than the screen be dragged up past the top", () => {
    // NEO's tool column against a phone in landscape. Pinning it below the
    // chrome would put its bottom out of reach for good, so the range inverts
    // and the only limit left is that its bottom edge stays on screen.
    const tall = window.innerHeight + 200;
    const { handle, positions } = mount(70, tall);

    drag(handle, { x: 10, y: 100 }, { x: 10, y: -500 });

    expect(positions.at(-1)?.y).toBe(window.innerHeight - tall);
    expect(positions.at(-1)?.y).toBeLessThan(0);
  });
});

describe("the bounds a window is held inside", () => {
  const layout = () => ({
    width: document.documentElement.clientWidth,
    height: document.documentElement.clientHeight,
  });

  it("follows the visual viewport at rest, which is what the URL bar moves", () => {
    // Shorter than the layout viewport, as when mobile Safari's bar slides in.
    const visual = { width: 390, height: 500, scale: 1 };

    expect(windowBounds(visual)).toEqual({ width: 390, height: 500 });
  });

  it("ignores the visual viewport while pinched, since fixed windows are not", () => {
    // Pinch zoom shrinks the visual viewport and leaves the layout viewport --
    // which is what `position: fixed` is placed against -- untouched. Following
    // it here pulled every panel in to the edge of the magnified region.
    const pinched = { width: 195, height: 332, scale: 2 };

    expect(windowBounds(pinched)).toEqual(layout());
  });

  it("ignores it when zoomed out as well", () => {
    expect(windowBounds({ width: 900, height: 1400, scale: 0.5 })).toEqual(layout());
  });

  it("falls back to the layout viewport when there is no visual viewport", () => {
    expect(windowBounds(null)).toEqual(layout());
  });
});
