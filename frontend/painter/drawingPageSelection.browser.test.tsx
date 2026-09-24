import { afterEach, describe, expect, it } from "vitest";
import { act } from "react";
import { userEvent } from "vitest/browser";
import { mount } from "neo-cucumber";
// This page's own stylesheet: /draw, both banner pages and the two-tone
// painter all serve what it compiles to. The collaborative page carries the
// same rules in its own.
import "./painter.css";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * A drawing page has nothing to read, so nothing on it is selectable -- except
 * where you type.
 *
 * This is NEO's arrangement: `.NEO` refuses selection and the touch callout,
 * `container.js` widens the refusal to the whole page, and `*[contenteditable]`
 * is the one thing let back in. Ours has to keep that exception working,
 * because the painter's text tool is a contenteditable sitting on the canvas --
 * and the canvas around it is refused as well.
 */

let host: HTMLElement | null = null;
let painter: ReturnType<typeof mount> | null = null;

async function mountPainter() {
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
  return area;
}

afterEach(() => {
  if (painter) act(() => painter?.unmount());
  painter = null;
  host?.remove();
  host = null;
  document.getSelection()?.removeAllRanges();
});

describe("a drawing page", () => {
  it("selects nothing when a drag crosses the toolbox", async () => {
    await mountPainter();
    const extras = document.querySelector<HTMLElement>(".toolbox-extras")!;
    // From the zoom readout, the extras column's one run of text, across
    // its neighbour.
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

  it("still lets the text tool be typed into and selected", async () => {
    const area = await mountPainter();
    const canvas = area.querySelector<HTMLCanvasElement>("#canvas")!;

    // T, then a press on the canvas, is how the text tool opens its editor.
    await act(async () => {
      await userEvent.keyboard("t");
      await userEvent.click(canvas, { position: { x: 100, y: 100 } });
    });

    const editor = area.querySelector<HTMLElement>("[contenteditable]");
    expect(editor).not.toBeNull();
    expect(getComputedStyle(editor!).userSelect).toBe("text");
  });
});
