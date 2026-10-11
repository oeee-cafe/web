import { act, useState } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";
import { NeoSizeSlider } from "./NeoSizeSlider";
import { mount } from "../../public";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * NEO gives thirty sizes 33 pixels of slider, which a pen cannot hit one at a
 * time. The press still lands on a size the way NEO's does; everything after
 * it walks one size per seven pixels, so these drive the gesture and read
 * back what it chose.
 */
describe("the brush size slider", () => {
  const cleanups: (() => void)[] = [];
  afterEach(() => {
    for (const cleanup of cleanups.splice(0)) cleanup();
  });

  const render = async (initial: number) => {
    const values: number[] = [];
    function Harness() {
      const [value, setValue] = useState(initial);
      return (
        <NeoSizeSlider
          value={value}
          color="#000000"
          onChange={(next) => {
            values.push(next);
            setValue(next);
          }}
        />
      );
    }
    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = createRoot(host);
    await act(async () => root.render(<Harness />));
    cleanups.push(() => {
      act(() => root.unmount());
      host.remove();
    });
    const hit = host.querySelector('[role="slider"]') as HTMLElement;
    // Synthetic pointers are not in Chromium's table of active ones
    hit.setPointerCapture = () => {};
    const top = hit.getBoundingClientRect().top;
    const now = () => Number(hit.getAttribute("aria-valuenow"));
    return { hit, top, now, values };
  };

  const pointer = (hit: HTMLElement, type: string, clientY: number) =>
    act(async () => {
      hit.dispatchEvent(new PointerEvent(type, {
        pointerId: 7, pointerType: "pen", button: 0, buttons: 1,
        clientX: 0, clientY, bubbles: true,
      }));
    });

  it("lands the press where NEO does, then walks a size per seven pixels", async () => {
    const { hit, top, now } = await render(3);

    // floor((15.5 - 4) * 30 / 33) is 10, exactly as NEO reads it
    await pointer(hit, "pointerdown", top + 15.5);
    expect(now()).toBe(10);

    await pointer(hit, "pointermove", top + 15.5 + 7);
    expect(now()).toBe(11);
    // A pen's wobble moves nothing
    await pointer(hit, "pointermove", top + 15.5 + 9);
    expect(now()).toBe(11);
    // Further down the box is no longer a jump to the size drawn there
    await pointer(hit, "pointermove", top + 35);
    expect(now()).toBe(13);
    await pointer(hit, "pointermove", top + 15.5 - 14);
    expect(now()).toBe(8);

    await pointer(hit, "pointerup", top + 15.5 - 14);
    await pointer(hit, "pointermove", top + 40);
    expect(now()).toBe(8);
  });

  it("steps one size a notch on the wheel, up for bigger, and keeps the page still", async () => {
    const { hit, now } = await render(10);
    const wheel = (deltaY: number, deltaMode = 0) => {
      const event = new WheelEvent("wheel", {
        deltaY, deltaMode, bubbles: true, cancelable: true,
      });
      act(() => {
        hit.dispatchEvent(event);
      });
      return event;
    };

    expect(wheel(-100).defaultPrevented).toBe(true);
    expect(now()).toBe(11);
    // Firefox's notch, in lines
    wheel(3, 1);
    expect(now()).toBe(10);

    // A trackpad's small deltas have to add up to a step
    wheel(10);
    wheel(10);
    wheel(10);
    expect(now()).toBe(10);
    wheel(10);
    expect(now()).toBe(9);
  });
});

describe("Ctrl+Alt drag on the canvas", () => {
  it("sizes the brush a size per seven pixels, as the slider does, and draws nothing", async () => {
    const element = document.createElement("div");
    document.body.appendChild(element);
    const operations: unknown[] = [];
    let painter!: ReturnType<typeof mount>;
    act(() => {
      painter = mount(element, {
        width: 64, height: 48,
        mode: { kind: "standard" },
        controls: { kind: "toolbox" },
        synchronization: { actorId: "1", onOperation: (op) => operations.push(op) },
      });
    });
    await act(async () => painter.ready);
    act(() => painter.setLocalActorId("1"));

    const canvas = element.querySelector("#canvas") as HTMLCanvasElement;
    const box = canvas.getBoundingClientRect();
    const slider = element.querySelector('[role="slider"][aria-label="Brush size"]')!;
    const size = () => Number(slider.getAttribute("aria-valuenow"));
    const before = size();
    const x = box.left + box.width / 2;
    const y = box.top + box.height / 2;
    const send = async (type: string, clientY: number) => {
      await act(async () => {
        canvas.dispatchEvent(new PointerEvent(type, {
          pointerId: 1, pointerType: "mouse", button: 0, buttons: 1,
          ctrlKey: true, altKey: true, clientX: x, clientY, bubbles: true,
        }));
      });
    };

    await send("pointerdown", y);
    await send("pointermove", y + 35);
    expect(size()).toBe(before + 5);
    // A trackpad's few pixels of drift moves nothing
    await send("pointermove", y + 37);
    expect(size()).toBe(before + 5);
    await send("pointermove", y + 14);
    expect(size()).toBe(before + 2);
    // And never below the smallest
    await send("pointermove", y - 350);
    expect(size()).toBe(1);
    await send("pointermove", y + 21);
    await send("pointerup", y + 21);
    await act(async () => new Promise((resolve) => setTimeout(resolve, 60)));

    expect(size()).toBe(before + 3);
    expect(operations).toEqual([]);

    act(() => painter.unmount());
    element.remove();
  });
});
