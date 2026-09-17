import { describe, expect, it } from "vitest";
import { act, useState } from "react";
import { createRoot } from "react-dom/client";
import { I18nProvider } from "@lingui/react";
import { i18n } from "@lingui/core";
import { NeoLayerControl } from "./NeoLayerControl";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * The layer button carries two gestures on one 48x20 target, and a touch
 * screen spells the second one with the first: a long press is an ordinary
 * press followed by `contextmenu`. Taking the swap on the press therefore made
 * a long press swap the layer and then hide the layer it had swapped to, which
 * leaves a painter that reads the right layer, accepts every stroke, records
 * every stroke, and shows nothing.
 */
function mountControl() {
  i18n.load("en", {});
  i18n.activate("en");
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);

  let seen!: { current: string; fgVisible: boolean; bgVisible: boolean };

  function Harness() {
    const [current, setCurrent] = useState<"foreground" | "background">("background");
    const [fgVisible, setFgVisible] = useState(true);
    const [bgVisible, setBgVisible] = useState(true);
    seen = { current, fgVisible, bgVisible };
    return (
      <NeoLayerControl
        current={current}
        fgVisible={fgVisible}
        bgVisible={bgVisible}
        onSwitch={() =>
          setCurrent((was) => (was === "foreground" ? "background" : "foreground"))
        }
        onToggleVisible={() =>
          current === "foreground" ? setFgVisible((on) => !on) : setBgVisible((on) => !on)
        }
      />
    );
  }

  act(() =>
    root.render(
      <I18nProvider i18n={i18n}>
        <Harness />
      </I18nProvider>,
    ),
  );
  const button = container.querySelector("button")!;

  const send = (type: string, init: PointerEventInit = {}) =>
    act(() => {
      button.dispatchEvent(
        new PointerEvent(type, {
          pointerId: 1,
          pointerType: "touch",
          button: 0,
          bubbles: true,
          cancelable: true,
          ...init,
        }),
      );
    });

  const contextMenu = () =>
    act(() => {
      button.dispatchEvent(
        new MouseEvent("contextmenu", { bubbles: true, cancelable: true }),
      );
    });

  return {
    button,
    send,
    contextMenu,
    state: () => seen,
    cleanup: () => {
      act(() => root.unmount());
      container.remove();
    },
  };
}

describe("the layer button", () => {
  it("swaps layers on a plain press and release", () => {
    const control = mountControl();
    control.send("pointerdown");
    control.send("pointerup");
    expect(control.state().current).toBe("foreground");
    expect(control.state().fgVisible).toBe(true);
    expect(control.state().bgVisible).toBe(true);
    control.cleanup();
  });

  it("hides the layer it names, and does not swap, on a long press", () => {
    const control = mountControl();
    control.send("pointerdown");
    control.contextMenu();
    control.send("pointerup");

    // The layer you were on is the one that hides -- not one you were moved to
    // without asking.
    expect(control.state().current).toBe("background");
    expect(control.state().bgVisible).toBe(false);
    expect(control.state().fgVisible).toBe(true);
    control.cleanup();
  });

  it("hides without swapping on a right press", () => {
    const control = mountControl();
    control.send("pointerdown", { pointerType: "mouse", button: 2 });
    control.contextMenu();
    control.send("pointerup", { pointerType: "mouse", button: 2 });

    expect(control.state().current).toBe("background");
    expect(control.state().bgVisible).toBe(false);
    control.cleanup();
  });

  it("strikes through a hidden layer's half of the button", () => {
    const control = mountControl();
    expect(control.button.querySelectorAll("div")).toHaveLength(1);
    control.send("pointerdown");
    control.contextMenu();
    expect(control.button.querySelectorAll("div")).toHaveLength(2);
    control.cleanup();
  });
});
