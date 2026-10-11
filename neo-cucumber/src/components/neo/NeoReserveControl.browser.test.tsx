import { act } from "react";
import { describe, expect, it } from "vitest";
import { mount } from "../../public";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * A slot saves on whatever NEO's `getModifier` calls a right press -- the
 * button, Ctrl, Alt, or the toolbox's sticky right-click button -- so a pen
 * with no second button can keep a size in one.
 */
describe("the tool memories", () => {
  it("save on a sticky right press, then let the button go", async () => {
    const element = document.createElement("div");
    document.body.appendChild(element);
    let painter!: ReturnType<typeof mount>;
    act(() => {
      painter = mount(element, {
        width: 64, height: 48,
        mode: { kind: "standard" },
        controls: { kind: "toolbox" },
        synchronization: { actorId: "1", onOperation: () => {} },
      });
    });
    await act(async () => painter.ready);

    const size = () =>
      Number(
        element
          .querySelector('[role="slider"][aria-label="Brush size"]')!
          .getAttribute("aria-valuenow"),
      );
    const setSize = (n: number) => {
      // ] steps it, and the toolbox reads the same state
      while (size() !== n) {
        const key = size() < n ? "]" : "[";
        act(() => {
          window.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
        });
      }
    };
    const slot = () =>
      element.querySelectorAll<HTMLElement>('button[title^="Slot "]')[1];
    const press = (init: PointerEventInit = {}) =>
      act(() => {
        slot().dispatchEvent(new PointerEvent("pointerdown", {
          pointerId: 1, pointerType: "pen", button: 0, buttons: 1, bubbles: true,
          ...init,
        }));
      });
    const sticky = element.querySelector<HTMLElement>(
      'button[aria-label="Right click"]',
    )!;

    setSize(9);
    act(() => sticky.click());
    expect(sticky.getAttribute("aria-pressed")).toBe("true");
    press();
    expect(sticky.getAttribute("aria-pressed")).toBe("false");

    setSize(2);
    press();
    expect(size()).toBe(9);

    // Ctrl and Alt are right presses too, and Shift is not
    setSize(4);
    press({ ctrlKey: true });
    setSize(6);
    press({ shiftKey: true, ctrlKey: true });
    expect(size()).toBe(4);
    setSize(12);
    press({ altKey: true });
    setSize(1);
    press();
    expect(size()).toBe(12);

    act(() => painter.unmount());
    element.remove();
  });
});
