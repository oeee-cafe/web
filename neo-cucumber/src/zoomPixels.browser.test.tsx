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

/**
 * One notch of the wheel over the canvas, at a deliberately fractional point
 * so the zoom's pointer anchoring produces a fractional pan.
 */
async function wheel(root: HTMLElement, deltaY: number) {
  const canvas = root.querySelector<HTMLCanvasElement>("#canvas")!;
  await act(async () => {
    canvas.dispatchEvent(
      new WheelEvent("wheel", {
        deltaY,
        clientX: 333.3,
        clientY: 222.7,
        bubbles: true,
        cancelable: true,
      }),
    );
  });
  // The pan that anchors the zoom is applied a frame later.
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
