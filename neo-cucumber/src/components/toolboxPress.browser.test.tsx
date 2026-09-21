import { afterEach, describe, expect, it } from "vitest";
import { act } from "react";
import { userEvent } from "vitest/browser";
import { mount } from "../public";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * A press on a floating panel has to reach the control it landed on.
 *
 * Twice it did not. A drag across the panel selects its text if the panel
 * lets itself be selected, and while a selection is up the browser spends the
 * next press dismissing it -- so one stray drag on the toolbox left every
 * button after it dead, for good, since nothing in a panel of buttons clears
 * a selection.
 *
 * It did not, with a stylus. A pen and a finger are given to the browser's
 * gesture recognizer before they are given to the page, and on a surface that
 * has not refused it a press that slides a pixel or two is read as the start
 * of a scroll: the tap is cancelled and no `click` is ever fired. NEO's own
 * column never noticed -- every control in it acts on `pointerdown` -- but our
 * extras column and the buttons a host appends to it all act on `click`, so
 * for a tablet they simply stopped working partway through a drawing.
 *
 * `touch-action: none` is what refuses it, and the rule the browser applies is
 * the intersection down the ancestor chain: it is enough for the window to say
 * it, which is why the window is where it is said.
 */

let host: HTMLElement | null = null;
let painter: ReturnType<typeof mount> | null = null;

async function mountPainter(mode: Parameters<typeof mount>[1]["mode"]) {
  const area = document.createElement("div");
  area.style.cssText = "position:absolute;inset:0";
  document.body.appendChild(area);
  host = area;
  act(() => {
    painter = mount(area, {
      width: 200,
      height: 200,
      mode,
      controls: { kind: "toolbox" },
    });
  });
  await act(async () => painter?.ready);
  return area;
}

afterEach(() => {
  if (painter) act(() => painter?.unmount());
  painter = null;
  host?.remove();
  host = null;
});

/** What the browser is left able to claim for a press landing on `element`. */
function gestureLeftToTheBrowser(element: Element): string {
  for (let node: Element | null = element; node; node = node.parentElement) {
    const touchAction = getComputedStyle(node).touchAction;
    if (touchAction === "none") return "none";
    if (node.classList.contains("shadow-lg")) break;
  }
  return getComputedStyle(element).touchAction;
}

const panel = (section: string) =>
  document.querySelector<HTMLElement>(`.toolbox-${section}`)!;

describe("pressing a control in a floating panel", () => {
  it("leaves the browser no gesture to claim, in either column", async () => {
    await mountPainter({ kind: "standard" });

    for (const section of ["neo", "extras"]) {
      const controls = panel(section).querySelectorAll("button, input");
      expect(controls.length).toBeGreaterThan(0);
      for (const control of controls) {
        expect(gestureLeftToTheBrowser(control)).toBe("none");
        expect(getComputedStyle(control).userSelect).toBe("none");
      }
    }
  });

  it("covers the buttons a host appends to the extras column", async () => {
    // What both pages do with their Save and Help: append them to the column
    // the painter renders, which is the only reason they wear its chrome.
    const area = await mountPainter({ kind: "standard" });
    const column = area.querySelector<HTMLInputElement>(
      'input[type="color"]',
    )!.parentElement!;

    const save = document.createElement("button");
    save.type = "button";
    column.append(save);

    expect(gestureLeftToTheBrowser(save)).toBe("none");
  });

  it("has no text to select, so a drag across it cannot arm one", async () => {
    // The drag that broke it: from a label in the extras column across its
    // neighbours. The zoom readout is the column's one run of text now.
    await mountPainter({ kind: "standard" });
    const extras = panel("extras");
    const readout = extras.querySelector<HTMLButtonElement>(
      'button[title="Reset zoom"]',
    )!;
    const undo = extras.querySelector<HTMLButtonElement>(
      'button[title="Undo"]',
    )!;

    await act(async () => {
      await userEvent.dragAndDrop(readout, undo, {
        sourcePosition: { x: 4, y: 7 },
        targetPosition: { x: 20, y: 8 },
      });
    });

    expect(String(document.getSelection())).toBe("");
  });

  it("still clicks, which is what the gesture was swallowing", async () => {
    await mountPainter({ kind: "standard" });
    const extras = panel("extras");
    const readout = () =>
      extras
        .querySelector<HTMLButtonElement>('button[title="Reset zoom"]')!
        .textContent!.trim();

    const before = readout();
    await act(async () => {
      await userEvent.click(
        extras.querySelector<HTMLButtonElement>('button[title="Zoom in"]')!,
      );
    });

    expect(readout()).not.toBe(before);
  });

  it("still lets a window that scrolls be scrolled", async () => {
    // The two-tone toolbox is the one panel tall enough to need it, so it
    // keeps `pan-y`: dragging it up and down is a gesture it does owe.
    await mountPainter({
      kind: "two-tone",
      backgroundColor: "#ffffff",
      foregroundColor: "#000000",
    });
    const frame = [...document.querySelectorAll<HTMLElement>("body div")].find(
      (el) =>
        getComputedStyle(el).position === "fixed" &&
        el.className.includes("shadow-lg"),
    )!;

    expect(getComputedStyle(frame).touchAction).toBe("pan-y");
  });
});
