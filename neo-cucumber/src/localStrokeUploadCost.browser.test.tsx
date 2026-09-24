import { afterEach, describe, expect, it } from "vitest";
import { act, createRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nProvider } from "@lingui/react";
import { i18n } from "@lingui/core";
import Painter from "./Painter";
import { DefaultI18n } from "./components/DefaultI18n";
import { PainterLabelContext } from "./hooks/usePainterLabels";
import { setupI18n } from "./utils/i18n";
import type { PainterHandle } from "./public";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * What a segment of the local stroke costs in uploads to the screen.
 *
 * Every segment used to re-upload every participant's two layers whole, and
 * synchronously, on top of the dirty rectangle the engine had already queued
 * for the frame. So a stroke got slower with each person who joined the room
 * -- four participants on a large canvas was hundreds of megabytes through
 * `putImageData` a second, on the thread following the pen. A segment writes
 * a few pixels, and a few pixels is what should go up.
 */

const WIDTH = 256;
const HEIGHT = 192;

let host: HTMLElement | null = null;
let root: Root | null = null;

afterEach(() => {
  act(() => root?.unmount());
  root = null;
  host?.remove();
  host = null;
});

const remoteDot = (x: number, y: number) => ({
  kind: "stroke" as const,
  layer: "foreground" as const,
  brushSize: 1,
  brush: "solid" as const,
  color: { r: 20, g: 30, b: 40, a: 255 },
  points: [{ x, y }],
  mask: { type: 0, r: 0, g: 0, b: 0 },
});

const nextFrame = () =>
  new Promise((resolve) => requestAnimationFrame(() => setTimeout(resolve, 0)));

describe("the cost of a local segment", () => {
  it("does not upload whole layers while the local user draws in a busy room", async () => {
    setupI18n("en");
    const area = document.createElement("div");
    area.style.cssText = "position:absolute;inset:0";
    document.body.appendChild(area);
    host = area;

    const handle = createRef<PainterHandle>();
    root = createRoot(area);
    act(() => {
      root!.render(
        <I18nProvider i18n={i18n} defaultComponent={DefaultI18n}>
          <PainterLabelContext.Provider value={undefined}>
            <Painter
              ref={handle}
              config={{
                width: WIDTH,
                height: HEIGHT,
                mode: { kind: "standard" },
                controls: { kind: "toolbox" },
                recordReplay: false,
                synchronization: { actorId: "1", onOperation: () => {} },
              }}
            />
          </PainterLabelContext.Provider>
        </I18nProvider>,
      );
    });
    await act(async () => handle.current!.ready);
    act(() => handle.current!.setLocalActorId("1"));

    // Three other people, each with a pair of layers on screen.
    await act(async () => {
      let sequence = 1;
      for (const actorId of ["2", "3", "4"]) {
        await handle.current!.applyCanonicalOperation({
          id: `dot-${actorId}`, actorId, sequence: sequence++,
          operation: remoteDot(10, 10),
        });
      }
      window.dispatchEvent(new Event("resize"));
      await new Promise((resolve) => setTimeout(resolve, 30));
      await nextFrame();
    });

    const canvas = area.querySelector("#canvas") as HTMLCanvasElement;
    const box = canvas.getBoundingClientRect();
    const scale = box.width / WIDTH;
    const at = (type: string, x: number, y: number) =>
      canvas.dispatchEvent(
        new PointerEvent(type, {
          pointerId: 1, pointerType: "mouse", button: 0,
          buttons: type === "pointerup" ? 0 : 1,
          clientX: box.left + x * scale, clientY: box.top + y * scale,
          bubbles: true,
        }),
      );

    // Counted at the prototype, so it sees every context whoever made it.
    const prototype = CanvasRenderingContext2D.prototype;
    const original = prototype.putImageData;
    let wholeLayers = 0;
    let pixels = 0;
    prototype.putImageData = function (
      this: CanvasRenderingContext2D,
      ...args: Parameters<typeof original>
    ) {
      const image = args[0];
      pixels += image.width * image.height;
      if (image.width === WIDTH && image.height === HEIGHT) wholeLayers += 1;
      return original.apply(this, args);
    } as typeof original;

    const SAMPLES = 40;
    try {
      await act(async () => {
        at("pointerdown", 40, 40);
        for (let i = 1; i <= SAMPLES; i++) {
          at("pointermove", 40 + i, 40 + i);
          if (i % 8 === 0) await nextFrame();
        }
        at("pointerup", 40 + SAMPLES, 40 + SAMPLES);
        await nextFrame();
      });
    } finally {
      prototype.putImageData = original;
    }

    expect(wholeLayers).toBe(0);
    // A short diagonal and its brush, a frame at a time: nowhere near even
    // one layer's worth, let alone eight.
    expect(pixels).toBeLessThan(WIDTH * HEIGHT);
    // And something did go up, or this proves nothing.
    expect(pixels).toBeGreaterThan(0);
  });
});
