import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { mount } from "./public";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * The gesture that killed the painter on tablets, through the real toolbox.
 *
 * Zoom out one rung, set the draw type to bezier, drag out a chord with a
 * stylus. The release used to throw -- the preview asked the pen kernel for
 * a 1px brush at 92%, a width it has no round for -- and the throw left the
 * pointer latched. A stylus gets a fresh pointer id on every contact, so the
 * taps that should place the handles were refused as a second pointer and
 * the curve never committed; nothing did, until a reload.
 */

let host: HTMLElement | null = null;
let painter: ReturnType<typeof mount> | null = null;
const surfaced: unknown[] = [];
const onError = (event: ErrorEvent) => {
  surfaced.push(event.error);
  event.preventDefault();
};

beforeEach(() => {
  surfaced.length = 0;
  window.addEventListener("error", onError);
});

afterEach(() => {
  window.removeEventListener("error", onError);
  if (painter) act(() => painter?.unmount());
  painter = null;
  host?.remove();
  host = null;
});

async function paintedPixels(): Promise<number> {
  const png = await painter!.exportPng();
  const bitmap = await createImageBitmap(png);
  const canvas = document.createElement("canvas");
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  const ctx = canvas.getContext("2d", { willReadFrequently: true })!;
  ctx.drawImage(bitmap, 0, 0);
  const { data } = ctx.getImageData(0, 0, canvas.width, canvas.height);
  let painted = 0;
  for (let i = 0; i < data.length; i += 4) {
    if (data[i + 3] > 0 && (data[i] < 250 || data[i + 1] < 250 || data[i + 2] < 250)) painted++;
  }
  return painted;
}

describe("a bezier drawn with a stylus at a zoom below 100%", () => {
  it("still commits, and the release does not throw", async () => {
    const area = document.createElement("div");
    area.style.cssText = "position:absolute;inset:0";
    document.body.appendChild(area);
    host = area;
    act(() => {
      painter = mount(area, {
        width: 200,
        height: 200,
        mode: { kind: "standard" },
        controls: { kind: "toolbox" },
      });
    });
    await act(async () => painter?.ready);

    const press = async (button: HTMLElement) => {
      await act(async () => {
        button.dispatchEvent(new PointerEvent("pointerdown", {
          pointerId: 1, pointerType: "mouse", button: 0, buttons: 1,
          bubbles: true, cancelable: true,
        }));
        button.click();
      });
    };

    // One rung below 100%: the readout goes from 100% to 92%.
    const readout = document.querySelector<HTMLButtonElement>('button[title="Reset zoom"]')!;
    const zoomOut = document.querySelector<HTMLButtonElement>('button[title="Zoom out"]')!;
    await act(async () => { zoomOut.click(); });
    expect(readout.textContent!.trim()).not.toBe("100%");

    // The draw-type tip: freehand -> line -> bezier.
    const drawType = [...document.querySelectorAll<HTMLButtonElement>("button")]
      .find((b) => b.title.startsWith("How strokes are laid down"))!;
    await press(drawType);
    await press(drawType);
    expect(drawType.textContent).toContain("Bezier");

    const canvas = document.getElementById("canvas")!;
    const rect = canvas.getBoundingClientRect();
    let contact = 40;
    const stylus = async (type: string, x: number, y: number, pointerId: number) => {
      await act(async () => {
        canvas.dispatchEvent(new PointerEvent(type, {
          pointerId, pointerType: "pen", button: 0,
          buttons: type === "pointerup" ? 0 : 1,
          clientX: rect.left + x, clientY: rect.top + y,
          bubbles: true, cancelable: true,
        }));
      });
    };
    const tap = async (x: number, y: number) => {
      const id = contact++;
      await stylus("pointerdown", x, y, id);
      await stylus("pointerup", x, y, id);
    };

    // The chord, dragged out on one contact.
    const chord = contact++;
    await stylus("pointerdown", 20, 100, chord);
    await stylus("pointermove", 120, 100, chord);
    await stylus("pointerup", 120, 100, chord);
    // Then a handle each on two fresh contacts; the third release commits.
    await tap(40, 40);
    await tap(100, 40);

    expect(surfaced).toEqual([]);
    expect(await paintedPixels()).toBeGreaterThan(20);
  });
});
