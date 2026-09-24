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

/**
 * The page as `draw_post_cucumber.jinja` lays it out for a guest: the notice
 * saying where the drawing will be kept, then the painter filling the rest.
 *
 * The notice is the reason the page needs a rule of its own. The package
 * refuses selection inside its panels, so a drag across the toolbox proves
 * nothing about the page; the notice is text the package never sees, and only
 * the page's stylesheet keeps a drag off it.
 */
async function mountPainter() {
  const page = document.createElement("div");
  page.style.cssText =
    "position:absolute;inset:0;display:flex;flex-direction:column";
  const notices = document.createElement("ul");
  notices.className = "ds-notices";
  notices.style.cssText = "margin:0;padding:4px 8px;list-style:none";
  const notice = document.createElement("li");
  notice.className = "ds-notice";
  notice.textContent =
    "Your drawing is kept in this browser until you sign in and post it.";
  notices.appendChild(notice);
  const area = document.createElement("div");
  area.style.cssText = "flex:1 1 auto;min-height:0;position:relative";
  page.append(notices, area);
  document.body.appendChild(page);
  host = page;
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
  it("selects nothing when a drag crosses the page's own text", async () => {
    await mountPainter();
    const notice = document.querySelector<HTMLElement>(".ds-notice")!;

    // Along the notice, not from it into the toolbox: a drag that ends on a
    // panel selects nothing here with or without this page's rule, so it
    // could not tell the two apart. The toolbox's own refusal is the
    // package's to test, in toolboxPress.browser.test.tsx.
    await act(async () => {
      await userEvent.dragAndDrop(notice, notice, {
        sourcePosition: { x: 4, y: 6 },
        targetPosition: { x: 200, y: 6 },
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
