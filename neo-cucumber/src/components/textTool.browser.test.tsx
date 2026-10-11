import { afterEach, describe, expect, it, vi } from "vitest";
import { userEvent } from "vitest/browser";
import { act } from "react";
import { mount, type PainterHandle, type PainterOptions } from "../public";
import "../App.css";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/*
 * A collaborative host turns the text tool off: text is the one mark the
 * browser rasterises with each viewer's own fonts, so the same message can
 * draw different pixels for different people. Off means unreachable -- from
 * the toolbox, from its key, and from the help that lists the keys -- while
 * a painter left alone keeps all three.
 */

let painter: PainterHandle | null = null;
let element: HTMLElement | null = null;

afterEach(() => {
  if (painter) act(() => painter?.unmount());
  painter = null;
  element?.remove();
  element = null;
});

async function open(options: Partial<PainterOptions> = {}) {
  element = document.createElement("div");
  element.style.cssText = "position:fixed;inset:0";
  document.body.appendChild(element);
  act(() => {
    painter = mount(element!, {
      width: 200,
      height: 200,
      mode: { kind: "standard" },
      controls: { kind: "toolbox" },
      locale: "en",
      ...options,
    });
  });
  await act(async () => painter!.ready);
  await vi.waitFor(() => {
    if (!penTip()) throw new Error("no toolbox yet");
  });
}

/** NEO's Pen tip, which cycles Solid, Brush and Text. */
const penTip = () =>
  Array.from(document.querySelectorAll<HTMLElement>(".toolbox-neo [title]")).find((tip) =>
    tip.title.startsWith("Solid") || tip.title.includes("(Solid,"),
  );

/**
 * Presses on the canvas and says whether the text tool answered, by opening
 * its box to type into: the one thing only it does.
 */
const pressOpensTextBox = async () => {
  const canvas = element!.querySelector("#canvas") as HTMLCanvasElement;
  const box = canvas.getBoundingClientRect();
  await act(async () => {
    for (const type of ["pointerdown", "pointerup"]) {
      canvas.dispatchEvent(new PointerEvent(type, {
        pointerId: 1, pointerType: "mouse", bubbles: true, button: 0, buttons: type === "pointerdown" ? 1 : 0,
        clientX: box.left + box.width / 2, clientY: box.top + box.height / 2,
      }));
    }
    await new Promise((resolve) => setTimeout(resolve, 60));
  });
  return document.querySelector("[contenteditable]") !== null;
};

/** The keys the shortcut help lists, opened the way a person opens it. */
const listedKeys = async () => {
  await userEvent.click(document.querySelector<HTMLElement>('button[aria-label="Keyboard shortcuts"]')!);
  return Array.from(document.querySelectorAll("kbd"), (key) => key.textContent);
};

describe("the text tool", () => {
  it("is offered by default", async () => {
    await open();
    expect(penTip()!.title).toContain("Text");
    await userEvent.keyboard("t");
    await vi.waitFor(() => expect(penTip()!.title.startsWith("Text")).toBe(true));
    expect(await pressOpensTextBox()).toBe(true);
    expect(await listedKeys()).toContain("T");
  });

  it("is unreachable when the host turns it off", async () => {
    await open({ textTool: false });
    expect(penTip()!.title).not.toContain("Text");
    await userEvent.keyboard("t");
    expect(await pressOpensTextBox()).toBe(false);
    const keys = await listedKeys();
    expect(keys).toContain("B");
    expect(keys).not.toContain("T");
  });
});
