import { afterEach, describe, expect, it } from "vitest";
import { refuseSelection } from "./selection";

/**
 * `selectionchange` is dispatched after the task that changed the selection,
 * so each check waits a turn of the event loop.
 */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

let stop: (() => void) | null = null;
const added: Element[] = [];

function attach<T extends Element>(element: T): T {
  document.body.appendChild(element);
  added.push(element);
  return element;
}

afterEach(() => {
  stop?.();
  stop = null;
  document.getSelection()?.removeAllRanges();
  for (const element of added.splice(0)) element.remove();
});

describe("a selection on a drawing page", () => {
  it("is cleared as soon as it appears over anything that is not a field", async () => {
    const label = attach(document.createElement("span"));
    label.textContent = "Undo";
    stop = refuseSelection();

    document.getSelection()!.selectAllChildren(label);
    expect(document.getSelection()!.isCollapsed).toBe(false);

    await settle();
    expect(document.getSelection()!.isCollapsed).toBe(true);
  });

  it("is left alone inside a field, the text tool's box included", async () => {
    const box = attach(document.createElement("div"));
    box.contentEditable = "true";
    box.textContent = "hello";
    stop = refuseSelection();

    document.getSelection()!.selectAllChildren(box);
    await settle();
    expect(String(document.getSelection())).toBe("hello");
  });

  it("stops watching once told to", async () => {
    const label = attach(document.createElement("span"));
    label.textContent = "Redo";
    refuseSelection()();

    document.getSelection()!.selectAllChildren(label);
    await settle();
    expect(String(document.getSelection())).toBe("Redo");
  });
});
