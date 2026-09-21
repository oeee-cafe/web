import { afterEach, describe, expect, it } from "vitest";
import { act } from "react";
import { mount } from "./public";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/*
 * Whether artwork pixels stay square on screen at every zoom.
 *
 * Two things have to hold for a nearest-neighbour scale to keep a one pixel
 * line the same width along its length: the scale has to be a whole number,
 * and the canvas has to start on a whole device pixel. Either one missing is
 * the wobble -- most pixels one screen pixel wide and some two. Both are
 * checked here by measuring where the canvas actually landed, which is what
 * the eye sees, rather than by reading back the zoom the state says it has.
 *
 * The canvas and the viewport are odd-sized on purpose: flex centring and a
 * centre transform origin both produce half pixels from odd sizes, and an
 * even-sized fixture would pass without the snap.
 */

const WIDTH = 301;
const HEIGHT = 201;

const mounted: Array<() => void> = [];

afterEach(() => {
  while (mounted.length) mounted.pop()?.();
});

const frame = () =>
  new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

async function mountPainter() {
  const element = document.createElement("div");
  element.style.cssText =
    "position:fixed;left:0;top:0;width:801px;height:603px;display:flex";
  document.body.appendChild(element);

  let painter!: ReturnType<typeof mount>;
  act(() => {
    painter = mount(element, {
      width: WIDTH,
      height: HEIGHT,
      mode: { kind: "standard" },
      controls: { kind: "none" },
    });
  });
  await act(async () => painter.ready);
  mounted.push(() => {
    act(() => painter.unmount());
    element.remove();
  });
  return element;
}

/** The artwork canvas's box on screen after everything has settled. */
function artworkBox(root: HTMLElement): DOMRect {
  const canvas = root.querySelector<HTMLCanvasElement>(".canvas-content canvas");
  if (!canvas) throw new Error("no artwork canvas");
  return canvas.getBoundingClientRect();
}

/** A Chromium mouse notch; a trackpad sends a stream of far smaller ones. */
const NOTCH = 100;

function dispatchWheel(root: HTMLElement, deltaY: number, at = { x: 333.3, y: 222.7 }) {
  const canvas = root.querySelector<HTMLCanvasElement>("#canvas")!;
  canvas.dispatchEvent(
    new WheelEvent("wheel", {
      deltaY,
      clientX: at.x,
      clientY: at.y,
      bubbles: true,
      cancelable: true,
    }),
  );
}

/**
 * One notch of the wheel over the canvas, at a deliberately fractional point
 * so the zoom's pointer anchoring produces a fractional pan. `direction` is
 * -1 to zoom in and 1 to zoom out.
 */
async function wheel(root: HTMLElement, direction: number) {
  await act(async () => dispatchWheel(root, direction * NOTCH));
  await act(async () => {
    await frame();
    await frame();
  });
}

function expectSquarePixels(root: HTMLElement) {
  const box = artworkBox(root);
  const ratio = window.devicePixelRatio || 1;
  const zoom = box.width / WIDTH;

  expect(Number.isInteger(zoom)).toBe(true);
  expect(box.height).toBe(HEIGHT * zoom);
  expect(Number.isInteger(box.left * ratio)).toBe(true);
  expect(Number.isInteger(box.top * ratio)).toBe(true);
  return zoom;
}

describe("zoom keeps artwork pixels square", () => {
  it("steps through whole numbers above 1x, each on a whole pixel", async () => {
    const root = await mountPainter();

    const seen = [expectSquarePixels(root)];
    for (let i = 0; i < 3; i++) {
      await wheel(root, -1);
      seen.push(expectSquarePixels(root));
    }

    expect(seen).toEqual([1, 2, 3, 4]);
  });

  it("stays on a whole pixel on the way back down", async () => {
    const root = await mountPainter();
    for (let i = 0; i < 3; i++) await wheel(root, -1);

    for (let i = 0; i < 3; i++) {
      await wheel(root, 1);
      expectSquarePixels(root);
    }
    expect(artworkBox(root).width).toBe(WIDTH);
  });

  it("stays on a whole pixel after the window changes size", async () => {
    const root = await mountPainter();
    await wheel(root, -1);

    // An odd change in width moves a flex-centred frame by half a pixel.
    root.style.width = "800px";
    await act(async () => {
      window.dispatchEvent(new Event("resize"));
      await frame();
    });

    expectSquarePixels(root);
  });

  /*
   * Below 1x no scale keeps pixels hard -- nearest-neighbour at 0.5x keeps
   * every other row and drops the rest -- so the canvas is resampled
   * instead, and switches back the moment the zoom returns to 1x.
   */
  it("smooths below 1x and goes back to hard pixels at 1x", async () => {
    const root = await mountPainter();
    const container = root.querySelector<HTMLElement>(".canvas-container")!;
    const canvas = root.querySelector<HTMLCanvasElement>(".canvas-content canvas")!;

    expect(getComputedStyle(canvas).imageRendering).toBe("pixelated");

    await wheel(root, 1);
    expect(artworkBox(root).width).toBeLessThan(WIDTH);
    expect(container.classList.contains("canvas-downscaled")).toBe(true);
    expect(getComputedStyle(canvas).imageRendering).toBe("auto");

    await wheel(root, -1);
    expect(container.classList.contains("canvas-downscaled")).toBe(false);
    expect(getComputedStyle(canvas).imageRendering).toBe("pixelated");
  });
});

/*
 * Where the pointer is over the artwork, before and after a zoom.
 *
 * The scale and the pan that keeps this still used to land in different
 * frames, and the pan was computed for a top-left origin the frame does not
 * have, so the canvas swung away from the pointer on every notch. Measured
 * synchronously after the event: a correct position a frame later is still a
 * visible jump.
 */
describe("zoom stays under the pointer", () => {
  const artworkUnder = (root: HTMLElement, x: number, y: number) => {
    const box = artworkBox(root);
    return {
      x: ((x - box.left) / box.width) * WIDTH,
      y: ((y - box.top) / box.height) * HEIGHT,
    };
  };

  it("keeps the artwork point under the pointer on every notch, in the same frame", async () => {
    const root = await mountPainter();
    // Off centre, so a pan that ignored the anchor would show.
    const at = { x: 480.5, y: 260.25 };
    const ratio = window.devicePixelRatio || 1;

    for (const direction of [-1, -1, -1, 1, 1, 1, 1, 1]) {
      const before = artworkUnder(root, at.x, at.y);
      act(() => dispatchWheel(root, direction * NOTCH, at));
      const after = artworkUnder(root, at.x, at.y);
      const zoom = artworkBox(root).width / WIDTH;
      // The pixel snap may move the canvas by under one device pixel.
      const slack = 1 / ratio / zoom + 1e-6;
      expect(Math.abs(after.x - before.x)).toBeLessThanOrEqual(slack);
      expect(Math.abs(after.y - before.y)).toBeLessThanOrEqual(slack);
    }
  });

  it("takes a trackpad's stream of small deltas as one gesture, not a step each", async () => {
    const root = await mountPainter();
    // Fifteen events of a gentle two-finger scroll: 60 pixels, well short of
    // the halfway point to 2x. The old handler took fifteen rungs from it.
    act(() => {
      for (let i = 0; i < 15; i++) dispatchWheel(root, -4);
    });
    await act(async () => {
      await frame();
    });
    expect(artworkBox(root).width / WIDTH).toBe(1);

    act(() => {
      for (let i = 0; i < 15; i++) dispatchWheel(root, -4);
    });
    await act(async () => {
      await frame();
    });
    expect(artworkBox(root).width / WIDTH).toBe(2);
  });
});
